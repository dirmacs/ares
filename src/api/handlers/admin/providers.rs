//! Admin providers domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use super::*;


use crate::AppState;
use crate::llm::provider_registry::{ModelInfo, RuntimeProviderEntry};
use crate::types::{AppError, Result};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use sha2::Digest;

pub async fn list_models_handler(State(state): State<AppState>) -> Result<Json<Vec<ModelInfo>>> {
    Ok(Json(state.provider_registry.list_models()))
}

pub async fn reload_runtime_provider_registry(state: &AppState) -> Result<()> {
    let store = RuntimeProviderStore::new(state.tenant_db.pool());
    let providers = store.list_all().await?;
    let mut entries = Vec::with_capacity(providers.len());
    let mut names = Vec::with_capacity(providers.len());

    for provider in providers {
        let (headers, api_key) = runtime_provider_entry_headers_and_key(provider.headers.as_ref());

        names.push(provider.name);
        entries.push(RuntimeProviderEntry {
            tenant_id: provider.tenant_id,
            display_name: provider.display_name,
            provider_type: provider.provider_type,
            api_base: provider.api_base,
            auth_type: provider.auth_type,
            default_model: provider.default_model,
            headers,
            api_key,
            enabled: provider.enabled,
        });
    }

    state
        .provider_registry
        .reload_runtime_providers(entries, names);
    Ok(())
}

/// List all runtime providers (global / tenant-scoped).
pub async fn list_runtime_providers(
    State(state): State<AppState>,
) -> Result<Json<Vec<RuntimeProviderResponse>>> {
    let store = RuntimeProviderStore::new(state.tenant_db.pool());
    let providers = store.list_all().await?;
    let response: Vec<RuntimeProviderResponse> = providers.into_iter().map(|p| p.into()).collect();
    tracing::info!("Listed {} runtime providers", response.len());
    Ok(Json(response))
}

/// Get a single runtime provider by name.
pub async fn get_runtime_provider(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<RuntimeProviderScopeQuery>,
) -> Result<Json<RuntimeProviderResponse>> {
    let store = RuntimeProviderStore::new(state.tenant_db.pool());
    let provider = store
        .get_scoped(query.tenant_id.as_deref(), &name)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("runtime provider {name} not found")))?;
    tracing::info!("Retrieved runtime provider {}", name);
    Ok(Json(provider.into()))
}

/// Create or update a runtime provider.
pub async fn upsert_runtime_provider(
    State(state): State<AppState>,
    Json(mut req): Json<CreateRuntimeProviderRequest>,
) -> Result<Json<RuntimeProviderResponse>> {
    let store = RuntimeProviderStore::new(state.tenant_db.pool());
    preserve_redacted_runtime_provider_secret(&store, &mut req).await?;
    let provider = store.upsert(&req).await?;
    reload_runtime_provider_registry(&state).await?;
    tracing::info!("Upserted runtime provider {}", provider.name);
    Ok(Json(provider.into()))
}

/// Hard-delete a runtime provider by name.
pub async fn delete_runtime_provider(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<RuntimeProviderScopeQuery>,
) -> Result<StatusCode> {
    let store = RuntimeProviderStore::new(state.tenant_db.pool());
    let rows = store
        .delete_scoped(query.tenant_id.as_deref(), &name)
        .await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "runtime provider {name} not found"
        )));
    }
    reload_runtime_provider_registry(&state).await?;
    tracing::info!("Deleted runtime provider {}", name);
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{delete, get, post};
    axum::Router::new()
        .route("/providers/list_models_handler", get(list_models_handler))
        .route("/providers/list_runtime_providers", get(list_runtime_providers))
        .route("/providers/get_runtime_provider", get(get_runtime_provider))
        .route("/providers/upsert_runtime_provider", post(upsert_runtime_provider))
        .route("/providers/delete_runtime_provider", delete(delete_runtime_provider))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ares_cordis_core::Service;
pub struct AdminProvidersService;
impl Service for AdminProvidersService {}