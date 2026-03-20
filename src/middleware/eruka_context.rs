use axum::{extract::Request, middleware::Next, response::Response};
use std::time::Duration;
use tracing::warn;

/// Eruka context stored in request extensions
#[derive(Clone)]
pub struct ErukaContext(pub String);

/// Middleware that fetches completeness context from Eruka and stores it in request extensions
pub async fn eruka_context_middleware(mut req: Request, next: Next) -> Response {
    // Check if Eruka context is enabled
    let enabled = std::env::var("ERUKA_CONTEXT_ENABLED")
        .ok()
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(true);

    if !enabled {
        return next.run(req).await;
    }

    // Get Eruka API URL
    let eruka_api_url = std::env::var("ERUKA_API_URL")
        .unwrap_or_else(|_| "http://localhost:8081".to_string());

    // Get service key if available
    let service_key = std::env::var("ERUKA_SERVICE_KEY").ok();

    // Build request URL
    let url = format!("{}/api/v1/completeness", eruka_api_url);

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

    // Build and send request
    let mut request = client.get(&url);
    if let Some(key) = service_key {
        request = request.header("X-Service-Key", key);
    }

    match request.send().await {
        Ok(response) => {
            if response.status().is_success() {
                match response.text().await {
                    Ok(context_string) => {
                        // Store context in request extensions
                        let mut req_ext = req.extensions_mut();
                        req_ext.insert(ErukaContext(context_string));
                    }
                    Err(e) => {
                        warn!("Failed to read Eruka completeness response: {}", e);
                    }
                }
            } else {
                warn!("Eruka completeness request returned status: {}", response.status());
            }
        }
        Err(e) => {
            warn!("Eruka context request failed: {}", e);
        }
    }

    next.run(req).await
}
