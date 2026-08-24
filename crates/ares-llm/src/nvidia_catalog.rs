//! NVIDIA Model Catalog Cache
//!
//! Fetches the live model catalog from `https://integrate.api.nvidia.com/v1/models`,
//! caches it in memory, and supports periodic background refresh.

use arc_swap::ArcSwap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Configuration for the NVIDIA provider and catalog fetch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvidiaConfig {
    /// Environment variable holding the NVIDIA API key (default: `NVIDIA_API_KEY`).
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,

    /// Base URL for NVIDIA NIM API calls (default: `https://integrate.api.nvidia.com/v1`).
    #[serde(default = "default_api_base")]
    pub api_base: String,

    /// URL to fetch the model catalog (default: `https://integrate.api.nvidia.com/v1/models`).
    #[serde(default = "default_models_url")]
    pub models_url: String,

    /// Background refresh interval in seconds. `0` disables background refresh.
    #[serde(default = "default_catalog_refresh_seconds")]
    pub catalog_refresh_seconds: u64,

    /// Fallback model id used when the catalog is empty or fetch fails.
    #[serde(default = "default_default_model")]
    pub default_model: String,
}

impl Default for NvidiaConfig {
    fn default() -> Self {
        Self {
            api_key_env: default_api_key_env(),
            api_base: default_api_base(),
            models_url: default_models_url(),
            catalog_refresh_seconds: default_catalog_refresh_seconds(),
            default_model: default_default_model(),
        }
    }
}

fn default_api_key_env() -> String {
    "NVIDIA_API_KEY".to_string()
}

fn default_api_base() -> String {
    "https://integrate.api.nvidia.com/v1".to_string()
}

fn default_models_url() -> String {
    "https://integrate.api.nvidia.com/v1/models".to_string()
}

fn default_catalog_refresh_seconds() -> u64 {
    3600
}

fn default_default_model() -> String {
    "meta/llama-3.3-70b-instruct".to_string()
}

/// A single entry from the NVIDIA catalog endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Full model id, e.g. `meta/llama-3.3-70b-instruct`.
    pub id: String,

    /// Organization that owns the model, e.g. `meta`.
    pub owned_by: String,

    /// Unix timestamp when the model was created.
    pub created: i64,

    /// Derived quality score (0-100).
    #[serde(skip)]
    pub quality_score: u8,
}

/// In-memory cache of the NVIDIA model catalog.
///
/// `cfg` is wrapped in `Arc<ArcSwap<...>>` so the admin can hot-swap the
/// `api_key_env`, `api_base`, and `models_url` fields at runtime without
/// restarting the service. The actual API key is NOT cached here — it is
/// resolved at refresh time from either the env var named in `cfg.api_key_env`
/// or the fleet provider secrets override.
pub struct NvidiaCatalogCache {
    inner: ArcSwap<Vec<CatalogEntry>>,
    last_fetch: RwLock<Option<Instant>>,
    last_error: RwLock<Option<String>>,
    cfg: Arc<ArcSwap<NvidiaConfig>>,
}

/// Errors that can occur during catalog refresh.
#[derive(Debug, thiserror::Error)]
pub enum NvidiaCatalogError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON parse failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("API key not found in environment variable {0}")]
    MissingApiKey(String),
    #[error("NVIDIA API returned HTTP {status}: {body}")]
    BadStatus { status: u16, body: String },
}

/// NVIDIA API response shape.
#[derive(Debug, Deserialize)]
struct NvidiaModelsResponse {
    #[serde(default)]
    data: Vec<NvidiaModelItem>,
}

#[derive(Debug, Deserialize)]
struct NvidiaModelItem {
    id: String,
    #[serde(default)]
    owned_by: String,
    #[serde(default)]
    created: i64,
}

impl NvidiaCatalogCache {
    /// Create a new empty cache from configuration.
    pub fn new(cfg: NvidiaConfig) -> Self {
        Self {
            inner: ArcSwap::from_pointee(Vec::new()),
            last_fetch: RwLock::new(None),
            last_error: RwLock::new(None),
            cfg: Arc::new(ArcSwap::from_pointee(cfg)),
        }
    }

    /// Build from a pre-constructed `Arc<ArcSwap<NvidiaConfig>>`. The
    /// caller's wrapper is shared with the registry/admin endpoint so a
    /// hot-swap is visible to all readers (including `refresh`).
    pub fn from_arcswap(cfg: Arc<ArcSwap<NvidiaConfig>>) -> Self {
        Self {
            inner: ArcSwap::from_pointee(Vec::new()),
            last_fetch: RwLock::new(None),
            last_error: RwLock::new(None),
            cfg,
        }
    }

    /// Fetch the catalog from NVIDIA and update the cache.
    ///
    /// Returns the number of chat models that were stored.
    pub async fn refresh(&self) -> Result<usize, NvidiaCatalogError> {
        // Snapshot the current config. The reference is short-lived; if an
        // admin hot-swaps mid-refresh, the next refresh sees the new value.
        let cfg_snapshot = self.cfg.load_full();
        let cfg_ref: &NvidiaConfig = cfg_snapshot.as_ref();

        let api_key = std::env::var(&cfg_ref.api_key_env)
            .map_err(|_| NvidiaCatalogError::MissingApiKey(cfg_ref.api_key_env.clone()))?;

        // Client-level timeouts are belt-and-braces: the request below also
        // carries its own total timeout, but future fetch paths may not.
        let client = reqwest::ClientBuilder::new()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build reqwest client");
        let resp = client
            .get(&cfg_ref.models_url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(10))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(NvidiaCatalogError::BadStatus {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: NvidiaModelsResponse = resp.json().await?;

        let mut entries: Vec<CatalogEntry> = parsed
            .data
            .into_iter()
            .filter(|item| is_chat_model(&item.id))
            .map(|item| CatalogEntry {
                quality_score: quality_score_for(&item.id),
                id: item.id,
                owned_by: if item.owned_by.is_empty() {
                    "nvidia".to_string()
                } else {
                    item.owned_by
                },
                created: item.created,
            })
            .collect();

        // Stable sort by quality score descending
        entries.sort_by_key(|e| std::cmp::Reverse(e.quality_score));

        let count = entries.len();
        self.inner.store(Arc::new(entries));
        *self.last_fetch.write() = Some(Instant::now());
        *self.last_error.write() = None;

        info!("NVIDIA catalog refreshed with {} chat models", count);
        Ok(count)
    }

    /// Atomically replace the cached `NvidiaConfig` (e.g. after an admin
    /// updates `api_key_env`, `api_base`, or `models_url`). Subsequent
    /// `refresh()` calls use the new config.
    pub fn update_config(&self, new_cfg: NvidiaConfig) {
        self.cfg.store(Arc::new(new_cfg));
    }

    /// Borrow a read handle to the current `NvidiaConfig` for callers that
    /// need to introspect the live values.
    pub fn config_handle(&self) -> Arc<ArcSwap<NvidiaConfig>> {
        Arc::clone(&self.cfg)
    }

    /// Snapshot the current refresh interval (seconds). Returns 0 if disabled.
    pub fn refresh_seconds(&self) -> u64 {
        self.cfg.load().catalog_refresh_seconds
    }

    /// Return a snapshot of the currently cached entries.
    pub fn snapshot(&self) -> Vec<CatalogEntry> {
        self.inner.load_full().as_ref().clone()
    }

    /// Return the age of the last successful fetch, if any.
    pub fn last_fetch_age(&self) -> Option<Duration> {
        self.last_fetch.read().map(|t| t.elapsed())
    }

    /// Return the last error message, if any.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.read().clone()
    }

    /// Spawn a background Tokio task that refreshes the catalog periodically.
    ///
    /// Does nothing if `catalog_refresh_seconds` is `0`.
    pub fn start_background_refresh(self: Arc<Self>) {
        let seconds = self.cfg.load().catalog_refresh_seconds;
        if seconds == 0 {
            return;
        }

        tokio::spawn(async move {
            loop {
                // Re-read the interval on each tick so an admin hot-swap of
                // `catalog_refresh_seconds` takes effect on the next loop.
                let secs = self.cfg.load().catalog_refresh_seconds;
                if secs == 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(secs)).await;
                if let Err(e) = self.refresh().await {
                    warn!("NVIDIA catalog background refresh failed: {}", e);
                    *self.last_error.write() = Some(e.to_string());
                }
            }
        });
    }
}

/// Filter out non-chat models by id substring.
fn is_chat_model(id: &str) -> bool {
    let lower = id.to_lowercase();
    let denied = [
        "embed",
        "rerank",
        "retriev",
        "parse",
        "reward",
        "safety",
        "guard",
        "detect",
        "asr",
        "tts",
        "kosmos",
        "vila",
        "vision-encoder",
    ];
    !denied.iter().any(|d| lower.contains(d))
}

/// Compute a quality score (0-100) from a model id.
fn quality_score_for(id: &str) -> u8 {
    let lower = id.to_lowercase();
    let mut score: u8 = 75;

    // Vendor-based adjustments
    if lower.contains("qwen") {
        score = score.saturating_add(12);
    } else if lower.contains("llama-3.3") || lower.contains("llama-3.1") {
        score = score.saturating_add(10);
    } else if lower.contains("mistral") || lower.contains("codestral") {
        score = score.saturating_add(8);
    } else if lower.contains("gemma-3") || lower.contains("nemotron") {
        score = score.saturating_add(6);
    } else if lower.contains("glm")
        || lower.contains("phi")
        || lower.contains("step")
        || lower.contains("granite")
    {
        score = score.saturating_add(3);
    }

    // Size-based adjustments
    if lower.contains("405b") {
        score = score.saturating_add(10);
    } else if lower.contains("70b") {
        score = score.saturating_add(8);
    } else if lower.contains("32b") {
        score = score.saturating_add(5);
    } else if lower.contains("14b") {
        score = score.saturating_add(3);
    } else if lower.contains("8b") {
        score = score.saturating_add(2);
    }

    score.min(100)
}
