//! Admin fleet_secrets domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use std::sync::Arc;
use ::cordis::Context;
use super::*;


use crate::AppState;
use crate::db::audit_log;
use crate::types::{AppError, Result};
use axum::{
    Json,
    extract::{Path, State},
};
use sha2::Digest;

pub async fn upsert_fleet_provider(
    State(ctx): State<Arc<Context>>,
    Path(provider_name): Path<String>,
    Json(req): Json<FleetProviderUpsertRequest>,
) -> Result<Json<serde_json::Value>> {
    if provider_name.is_empty() {
        return Err(AppError::InvalidInput(
            "provider_name must not be empty".into(),
        ));
    }
    if req.api_key.is_none()
        && req.api_base.is_none()
        && req.default_model.is_none()
        && req.fallback_providers.is_none()
    {
        return Err(AppError::InvalidInput(
            "At least one of api_key, api_base, default_model, fallback_providers must be provided"
                .into(),
        ));
    }

    let __pool_1 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = fps::FleetProviderSecretsStore::new(&__pool_1);
    let master = MasterKey::from_env();
    if req.api_key.is_some() && master.is_none() {
        return Err(AppError::Configuration(
            "FLEET_SECRETS_KEY is not set; cannot store new API keys. \
             Configure /etc/dirmacs/fleet-secrets.env and reload ares.service."
                .into(),
        ));
    }

    let updated_by = "admin";
    let fallback_slice = req.fallback_providers.as_deref();
    let stored = store
        .upsert(
            &provider_name,
            req.api_key.as_deref(),
            req.api_base.as_deref(),
            req.default_model.as_deref(),
            fallback_slice,
            master.as_ref(),
            updated_by,
        )
        .await?;

    // Reload + atomically swap the in-memory map. The store gives us the
    // encrypted form; we need the decrypted form for the in-memory cache.
    let map = store.load_all(master.as_ref()).await?;
    ctx.get::<crate::FleetSecrets>().expect("not provided").store(map);

    // Audit log — redact the raw key, only emit the boolean + last-4.
    let details = serde_json::json!({
        "api_key_set": stored.has_api_key,
        "api_key_last4": req.api_key.as_deref().and_then(|k| last_n_visible(k, 4)),
        "api_base_set": stored.api_base.is_some(),
        "default_model_set": stored.default_model.is_some(),
        "fallback_providers": stored.fallback_providers,
    })
    .to_string();
    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let name = provider_name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "fleet_provider_upsert",
            "fleet_provider",
            &name,
            Some(&details),
            None,
        )
        .await;
    });

    Ok(Json(serde_json::json!({
        "name": provider_name,
        "has_api_key": stored.has_api_key,
        "api_base": stored.api_base,
        "default_model": stored.default_model,
        "fallback_providers": stored.fallback_providers,
        "updated_at": stored.updated_at,
        "updated_by": stored.updated_by,
    })))
}

/// Hard-delete a fleet provider override. Gone from the DB and from the
/// in-memory cache. Re-add via the UI to bring it back.
pub async fn delete_fleet_provider(
    State(ctx): State<Arc<Context>>,
    Path(provider_name): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let __pool_2 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = fps::FleetProviderSecretsStore::new(&__pool_2);
    let affected = store.delete(&provider_name).await?;
    if affected == 0 {
        return Err(AppError::NotFound(format!(
            "Fleet provider '{}' not found",
            provider_name
        )));
    }

    // Reload + atomically swap the in-memory map.
    let master = MasterKey::from_env();
    let map = store.load_all(master.as_ref()).await?;
    ctx.get::<crate::FleetSecrets>().expect("not provided").store(map);

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let name = provider_name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "fleet_provider_delete",
            "fleet_provider",
            &name,
            None,
            None,
        )
        .await;
    });

    Ok(Json(serde_json::json!({
        "name": provider_name,
        "deleted": true
    })))
}

/// Verify a stored provider's API key by calling its model listing endpoint.
/// For OpenAI-compatible providers we call `<api_base>/models` with the
/// stored key. For Anthropic/Ollama (when those features are enabled) the
/// call shape is different — the UI surfaces a clear error in that case
/// and tells the operator to test via the provider's own dashboard.
pub async fn verify_fleet_provider(
    State(ctx): State<Arc<Context>>,
    Path(provider_name): Path<String>,
) -> Result<Json<FleetProviderVerifyResponse>> {
    let start = std::time::Instant::now();

    // Look up the in-memory override (decrypted).
    let entry = ctx.get::<crate::FleetSecrets>().expect("not provided").get(&provider_name);
    let override_ = entry.clone();

    // Fall back to the registry's ProviderConfig for the base URL when the
    // admin hasn't set an override.
    let provider_config = ctx.get::<crate::ProviderRegistry>().expect("not provided").get_provider_for_ctx(&ctx, &provider_name);

    let (api_base, api_key) = match (override_, provider_config.as_ref()) {
        (Some(o), _) => {
            let key = o.api_key.clone();
            let base = o
                .api_base
                .clone()
                .or_else(|| provider_config.as_ref().and_then(default_api_base));
            (base, key)
        }
        (None, Some(pc)) => (default_api_base(pc), resolve_env_key(pc)),
        (None, None) => (None, None),
    };

    let api_key = match api_key {
        Some(k) if !k.is_empty() => k,
        _ => {
            return Ok(Json(FleetProviderVerifyResponse {
                name: provider_name,
                ok: false,
                latency_ms: start.elapsed().as_millis() as u64,
                model_count: 0,
                models: vec![],
                error: Some(
                    "No API key configured for this provider. Set one in the Fleet Providers card."
                        .into(),
                ),
            }));
        }
    };

    let api_base = match api_base {
        Some(b) if !b.is_empty() => b,
        _ => {
            return Ok(Json(FleetProviderVerifyResponse {
                name: provider_name,
                ok: false,
                latency_ms: start.elapsed().as_millis() as u64,
                model_count: 0,
                models: vec![],
                error: Some("No API base URL configured for this provider.".into()),
            }));
        }
    };

    let models_url = format!("{}/models", api_base.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = match client
        .get(&models_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("api-key", &api_key)
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return Ok(Json(FleetProviderVerifyResponse {
                name: provider_name,
                ok: false,
                latency_ms: start.elapsed().as_millis() as u64,
                model_count: 0,
                models: vec![],
                error: Some(format!("HTTP request failed: {e}")),
            }));
        }
    };

    let status = resp.status();
    let latency_ms = start.elapsed().as_millis() as u64;
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Ok(Json(FleetProviderVerifyResponse {
            name: provider_name,
            ok: false,
            latency_ms,
            model_count: 0,
            models: vec![],
            error: Some(format!("HTTP {} — {}", status.as_u16(), body)),
        }));
    }

    let parsed: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return Ok(Json(FleetProviderVerifyResponse {
                name: provider_name,
                ok: false,
                latency_ms,
                model_count: 0,
                models: vec![],
                error: Some(format!("JSON parse failed: {e}")),
            }));
        }
    };

    // OpenAI-compatible providers return `{"data": [{"id": "..."}]}`.
    // Pull ids out of `data[*].id`. If `data` is missing, return empty list.
    let models: Vec<String> = parsed
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("id").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let model_count = models.len();

    Ok(Json(FleetProviderVerifyResponse {
        name: provider_name,
        ok: true,
        latency_ms,
        model_count,
        models,
        error: None,
    }))
}

/// Return the list of supported provider types based on compiled-in
/// features. The admin UI uses this to filter the type dropdown so users
/// can only select providers that this build can actually instantiate.
// cordis Phase6: runtime gating via Service check — previously feature-gated
pub async fn fleet_provider_capabilities() -> Json<FleetProviderCapabilities> {
    let providers: Vec<&'static str> = vec!["openai", "azure", "anthropic", "bedrock", "ollama"];

    Json(FleetProviderCapabilities {
        providers,
        encryption_enabled: MasterKey::from_env().is_some(),
    })
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{delete, get, post};
    axum::Router::new()
        .route("/fleet_secrets/list_fleet_providers", get(list_fleet_providers))
        .route("/fleet_secrets/upsert_fleet_provider", post(upsert_fleet_provider))
        .route("/fleet_secrets/delete_fleet_provider", delete(delete_fleet_provider))
        .route("/fleet_secrets/verify_fleet_provider", post(verify_fleet_provider))
        .route("/fleet_secrets/fleet_provider_capabilities", get(fleet_provider_capabilities))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ::cordis::Service;
pub struct AdminFleetSecretsService;
impl Service for AdminFleetSecretsService {}
