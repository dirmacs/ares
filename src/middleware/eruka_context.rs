use axum::{
    body::Body,
    body::BodyExt,
    extract::Request,
    middleware::Next,
    response::Response,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{info, warn};

#[cfg(feature = "eruka-context")]
use std::path::Path;

/// Eruka context stored in request extensions
#[derive(Clone)]
pub struct ErukaContext(pub String);

/// Cache entry: context string and timestamp
#[derive(Clone)]
struct CacheEntry {
    context: String,
    timestamp: Instant,
}

/// Agent configuration: paths to fetch from Eruka
#[cfg(feature = "eruka-context")]
static AGENT_CONFIG: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();

/// Cache: agent_type -> (context_string, timestamp)
static CONTEXT_CACHE: OnceLock<RwLock<HashMap<String, CacheEntry>>> = OnceLock::new();

/// Circuit breaker: failure counter
static FAILURE_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Circuit breaker: last failure timestamp
static LAST_FAILURE_TIME: OnceLock<Instant> = OnceLock::new();

/// Cache TTL in seconds
const CACHE_TTL_SECS: u64 = 60;

/// Circuit breaker threshold
const CIRCUIT_BREAKER_THRESHOLD: u32 = 3;

/// Circuit breaker reset duration
const CIRCUIT_BREAKER_RESET_SECS: u64 = 60;

/// Get or initialize agent config from eruka-context.toml
#[cfg(feature = "eruka-context")]
fn get_agent_config() -> &'static HashMap<String, Vec<String>> {
    AGENT_CONFIG.get_or_init(|| {
        let config_path = Path::new("eruka-context.toml");
        
        if !config_path.exists() {
            warn!("eruka-context.toml not found, using empty config");
            return HashMap::new();
        }
        
        match std::fs::read_to_string(config_path) {
            Ok(contents) => {
                match toml::from_str::<toml::Value>(&contents) {
                    Ok(value) => {
                        let mut map = HashMap::new();
                        if let Some(agent_context) = value.get("agent_context").and_then(|v| v.as_table()) {
                            for (agent_type, paths) in agent_context {
                                if let Some(paths_array) = paths.as_array() {
                                    let path_strings: Vec<String> = paths_array
                                        .iter()
                                        .filter_map(|v| v.as_str().map(String::from))
                                        .collect();
                                    if !path_strings.is_empty() {
                                        map.insert(agent_type.clone(), path_strings);
                                    }
                                }
                            }
                        }
                        info!("Loaded agent config with {} agent types", map.len());
                        map
                    }
                    Err(e) => {
                        warn!("Failed to parse eruka-context.toml: {}", e);
                        HashMap::new()
                    }
                }
            }
            Err(e) => {
                warn!("Failed to read eruka-context.toml: {}", e);
                HashMap::new()
            }
        }
    })
}

/// Get the cache, initializing if needed
fn get_cache() -> &'static RwLock<HashMap<String, CacheEntry>> {
    CONTEXT_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Check if circuit breaker is open (too many recent failures)
fn is_circuit_open() -> bool {
    let failures = FAILURE_COUNTER.load(Ordering::Relaxed);
    
    if failures > CIRCUIT_BREAKER_THRESHOLD {
        // Check if we've waited long enough to try again
        if let Some(last_failure) = LAST_FAILURE_TIME.get() {
            if last_failure.elapsed() < Duration::from_secs(CIRCUIT_BREAKER_RESET_SECS) {
                return true;
            }
            // Reset the counter after the cooldown period
            FAILURE_COUNTER.store(0, Ordering::Relaxed);
        }
    }
    false
}

/// Record a failure for circuit breaker
fn record_failure() {
    FAILURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    LAST_FAILURE_TIME.get_or_init(|| Instant::now());
}

/// Record a success for circuit breaker
fn record_success() {
    FAILURE_COUNTER.store(0, Ordering::Relaxed);
}

/// Middleware that fetches per-agent context from Eruka and stores it in request extensions
pub async fn eruka_context_middleware(mut req: Request, next: Next) -> Response {
    // Check if Eruka context is enabled
    #[cfg(feature = "eruka-context")]
    let enabled = std::env::var("ERUKA_CONTEXT_ENABLED")
        .ok()
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(true);

    #[cfg(not(feature = "eruka-context"))]
    let enabled = false;

    if !enabled {
        return next.run(req).await;
    }

    let start_time = Instant::now();
    let mut cache_hit = false;
    let mut context_fields_count = 0;
    let mut total_chars = 0;
    let mut agent_type = "default".to_string();

    // Buffer the request body to extract agent_type
    let body_bytes = match to_bytes(req.body_mut(), 1_048_576).await {
        Ok(bytes) => bytes,
        Err(e) => {
            warn!("Failed to buffer request body: {}", e);
            return next.run(req).await;
        }
    };

    // Parse body as JSON to extract agent_type
    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
        if let Some(agent) = json.get("agent_type").and_then(|v| v.as_str()) {
            agent_type = agent.to_string();
        }
    }

    // Get agent-specific paths from config
    #[cfg(feature = "eruka-context")]
    let paths = {
        let config = get_agent_config();
        config.get(&agent_type).cloned().unwrap_or_else(|| {
            config.get("default").cloned().unwrap_or_else(|| {
                vec!["operations/framework_versions".to_string(), "operations/guardrails".to_string()]
            })
        })
    };

    #[cfg(not(feature = "eruka-context"))]
    let paths: Vec<String> = vec!["operations/framework_versions".to_string(), "operations/guardrails".to_string()];

    // Check cache first
    let cache = get_cache();
    let cached_context = {
        let cache_read = cache.read().await;
        cache_read.get(&agent_type).and_then(|entry| {
            if entry.timestamp.elapsed() < Duration::from_secs(CACHE_TTL_SECS) {
                Some(entry.context.clone())
            } else {
                None
            }
        })
    };

    let final_context = if let Some(cached) = cached_context {
        cache_hit = true;
        context_fields_count = paths.len();
        total_chars = cached.len();
        cached
    } else {
        // Check circuit breaker
        if is_circuit_open() {
            info!(
                agent_type = %agent_type,
                cache_hit = false,
                context_fields_count = 0,
                total_chars = 0,
                latency_ms = start_time.elapsed().as_millis(),
                "Circuit breaker open, skipping Eruka queries"
            );
            return next.run(req).await;
        }

        // Get Eruka API URL
        let eruka_api_url = std::env::var("ERUKA_API_URL")
            .unwrap_or_else(|_| "http://localhost:8081".to_string());

        // Get service key if available
        let service_key = std::env::var("ERUKA_SERVICE_KEY").ok();

        // Create HTTP client with 50ms timeout
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_millis(50))
            .build()
        {
            Ok(client) => client,
            Err(e) => {
                warn!("Failed to build reqwest client for Eruka: {}", e);
                return next.run(req).await;
            }
        };

        let mut context_parts: Vec<String> = Vec::new();
        let mut citation_counter = 1;
        let mut query_failed = false;

        // Query each path
        for path in &paths {
            let url = format!("{}/api/v1/context?path={}", eruka_api_url, path);
            
            let mut request = client.get(&url);
            if let Some(ref key) = service_key {
                request = request.header("X-Service-Key", key);
            }

            match request.send().await {
                Ok(response) => {
                    if response.status().is_success() {
                        if let Ok(text) = response.text().await {
                            // Format as: [E1] path: value (knowledge_state, confidence)
                            let fact = format!(
                                "[E{}] {}: {} (knowledge_state: available, confidence: 0.9)",
                                citation_counter,
                                path,
                                text
                            );
                            context_parts.push(fact);
                            citation_counter += 1;
                        }
                    } else {
                        warn!("Eruka context request returned status: {}", response.status());
                        query_failed = true;
                    }
                }
                Err(e) => {
                    warn!("Eruka context request failed for path {}: {}", path, e);
                    query_failed = true;
                }
            }
        }

        // Query gaps endpoint
        let gaps_url = format!("{}/api/v1/gaps", eruka_api_url);
        let mut gaps_request = client.get(&gaps_url);
        if let Some(ref key) = service_key {
            gaps_request = gaps_request.header("X-Service-Key", key);
        }

        let mut gaps_section = String::new();
        match gaps_request.send().await {
            Ok(response) => {
                if response.status().is_success() {
                    if let Ok(text) = response.text().await {
                        if !text.is_empty() {
                            gaps_section = format!("[GAPS — DO NOT FABRICATE THESE]\n{}", text);
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Eruka gaps request failed: {}", e);
            }
        }

        // Combine context parts
        let mut final_context_string = context_parts.join("\n");
        if !gaps_section.is_empty() {
            if !final_context_string.is_empty() {
                final_context_string.push_str("\n\n");
            }
            final_context_string.push_str(&gaps_section);
        }

        // Update circuit breaker
        if query_failed {
            record_failure();
        } else {
            record_success();
        }

        // Cache the result
        {
            let mut cache_write = cache.write().await;
            cache_write.insert(
                agent_type.clone(),
                CacheEntry {
                    context: final_context_string.clone(),
                    timestamp: Instant::now(),
                },
            );
        }

        context_fields_count = paths.len();
        total_chars = final_context_string.len();
        final_context_string
    };

    // Store context in request extensions
    let mut req_ext = req.extensions_mut();
    req_ext.insert(ErukaContext(final_context.clone()));

    // Reconstruct the body so downstream handlers can still read it
    *req.body_mut() = Body::from(body_bytes);

    // Log the operation
    info!(
        agent_type = %agent_type,
        cache_hit = cache_hit,
        context_fields_count = context_fields_count,
        total_chars = total_chars,
        latency_ms = start_time.elapsed().as_millis(),
        "Eruka context middleware completed"
    );

    next.run(req).await
}