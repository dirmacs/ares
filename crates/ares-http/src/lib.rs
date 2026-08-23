//! HTTP adapter plugin: Axum router, JWT auth, middleware.
//!
//! Overlay / toon / engines stay as files under `src/` (server tree) and are
//! compiled into this crate so handlers can `ctx.get` them without depending
//! on `ares-server`. Overlay *factory* registration stays in the server.

#![allow(missing_docs)]
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(hidden_glob_reexports)]
#![allow(ambiguous_glob_reexports)]
#![allow(private_interfaces)]
#![allow(clippy::useless_conversion)]
#![allow(clippy::unnecessary_sort_by)]
#![allow(clippy::empty_line_after_doc_comments)]
#![allow(clippy::too_many_arguments)]

use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use cordis::{Context, CordisError, Plugin, PluginRegistry, Service};

pub mod config;
pub mod error;
pub mod pipeline_hook;

#[path = "../../../src/overlay.rs"]
pub mod overlay;
#[path = "../../../src/toon_config.rs"]
pub mod toon_config;

#[cfg(feature = "postgres")]
pub mod api;
#[cfg(feature = "postgres")]
pub mod auth;
#[cfg(feature = "postgres")]
pub mod middleware;

#[cfg(feature = "postgres")]
#[path = "../../../src/active_runs.rs"]
pub mod active_runs;
#[cfg(feature = "postgres")]
#[path = "../../../src/observability.rs"]
pub mod observability;

#[cfg(feature = "postgres")]
pub use ares_agent::trigger as trigger_engine;
#[cfg(feature = "postgres")]
pub use ares_agent::skills as skill_engine;
#[cfg(any(feature = "postgres", feature = "skills"))]
pub use ares_agent::skills;
#[cfg(feature = "postgres")]
pub use ares_agent::workflows;
pub use ares_agent::EmergencyStop;

pub use config::{AuthConfig, ServerConfig};
pub use error::{app_error_into_response, HttpError};
pub use overlay::{
    AresConfig, AresConfigManager, ConfigError, Overlay, OverlayConfig, OverlayPlugin,
};
pub use toon_config::DynamicConfigManager;
#[cfg(feature = "postgres")]
pub use pipeline_hook::{PipelineFanout, PipelineFanoutHandle, PipelineOrigin};
pub use ares_types::{models, types};
pub use ares_llm::ConfigBasedLLMFactory;

/// Compatibility paths so moved handlers can keep `crate::agents` / `crate::db`.
pub use ares_agent as agents;
pub use ares_agent::memory;
#[cfg(feature = "postgres")]
pub use ares_agent::research;
pub mod db {
    pub use ares_store::*;
}
pub mod utils {
    pub use crate::overlay as toml_config;
    pub use crate::toon_config;
}

/// Handler result that maps [`ares_types::AppError`] through [`HttpError`].
pub type Result<T> = std::result::Result<T, HttpError>;

/// Built Axum router provided by the Http plugin. Bind happens in `run_server`.
pub struct Http {
    pub router: Router,
}

impl Service for Http {
    fn name(&self) -> &'static str {
        "http"
    }
}

/// Typed installer for [`Http`]. Host/port in [`ServerConfig`] are unused at apply
/// time; the binary binds `Http.router`.
pub struct HttpPlugin;

impl Plugin for HttpPlugin {
    type Config = ServerConfig;
    type Provides = Http;

    fn apply(
        &self,
        ctx: &Arc<Context>,
        _config: Self::Config,
    ) -> std::result::Result<Arc<Http>, CordisError> {
        Ok(Arc::new(Http {
            router: build_router(Arc::clone(ctx)),
        }))
    }
}

/// Cordis health routes used as the live HTTP base.
pub fn cordis_routes() -> Router<Arc<Context>> {
    Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/health/context", get(health_context))
}

async fn health_context(
    axum::extract::State(ctx): axum::extract::State<Arc<Context>>,
) -> impl axum::response::IntoResponse {
    let _events = ctx.get::<cordis::EventsService>();
    let _registry = ctx.get::<cordis::RegistryService>();
    let _exec = ctx.get::<ares_agent::Execute>();
    "OK"
}

/// Resolve an abstract model tier to `(provider, model)`.
#[cfg(feature = "postgres")]
pub async fn resolve_model_tier(
    tenant_id: &str,
    tier_name: &str,
    pool: &sqlx::PgPool,
    config: &AresConfig,
) -> Option<(String, String)> {
    let store = ares_store::tenant_model_tiers::TenantModelTierStore::new(pool);
    if let Ok(Some(tier)) = store.get(tenant_id, tier_name).await {
        return Some((tier.provider_name, tier.model_name));
    }
    config
        .models
        .get(tier_name)
        .map(|mc| (mc.provider.clone(), mc.model.clone()))
}

/// Build the application router from a Cordis context.
///
/// Nests `/api` when Auth + TenantDb are on the context. Does not bind a port.
pub fn build_router(ctx: Arc<Context>) -> Router {
    let mut app = cordis_routes();
    #[cfg(feature = "postgres")]
    {
        if let (Some(auth), Some(db)) = (
            ctx.get::<crate::auth::jwt::AuthService>(),
            ctx.get::<ares_store::TenantDb>(),
        ) {
            app = app.nest("/api", crate::api::routes::create_router(auth, db));
        }
    }
    let _ = ctx.get::<cordis::EventsService>();
    let _ = ctx.get::<cordis::RegistryService>();
    let _ = ctx.get::<ares_agent::Execute>();
    let _ = ctx.get::<ares_tools::Tools>();
    app.with_state(ctx)
}

fn block_on_plugin<S: Service + 'static>(
    ctx: &Arc<Context>,
    svc: S,
) -> std::result::Result<cordis::FiberId, CordisError> {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(ctx.plugin(svc)))
}

#[cfg(feature = "postgres")]
fn factory_auth(
    ctx: &Arc<Context>,
    config: &serde_json::Value,
) -> std::result::Result<cordis::FiberId, CordisError> {
    use crate::auth::jwt::AuthService;
    let auth = if let Some(mgr) = ctx.get::<AresConfigManager>() {
        let cfg = mgr.config();
        let jwt_secret = cfg.jwt_secret().map_err(|e| {
            CordisError::Configuration(format!(
                "JWT_SECRET environment variable must be set: {e}"
            ))
        })?;
        AuthService::new(
            jwt_secret,
            cfg.auth.jwt_access_expiry,
            cfg.auth.jwt_refresh_expiry,
        )
    } else {
        let auth_cfg: AuthConfig = if config.is_null()
            || config.as_object().is_some_and(|o| o.is_empty())
        {
            AuthConfig::default()
        } else {
            serde_json::from_value(config.clone()).map_err(|e| {
                CordisError::Configuration(format!("invalid AuthService config: {e}"))
            })?
        };
        let jwt_secret = std::env::var(&auth_cfg.jwt_secret_env).map_err(|_| {
            CordisError::Configuration(format!(
                "JWT_SECRET environment variable must be set ({})",
                auth_cfg.jwt_secret_env
            ))
        })?;
        AuthService::new(
            jwt_secret,
            auth_cfg.jwt_access_expiry,
            auth_cfg.jwt_refresh_expiry,
        )
    };
    tracing::info!("Auth service initialized");
    block_on_plugin(ctx, auth)
}

fn factory_http(
    ctx: &Arc<Context>,
    config: &serde_json::Value,
) -> std::result::Result<cordis::FiberId, CordisError> {
    let server_cfg: ServerConfig = if config.is_null()
        || config.as_object().is_some_and(|o| o.is_empty())
    {
        if let Some(mgr) = ctx.get::<AresConfigManager>() {
            mgr.config().server.clone()
        } else {
            ServerConfig::default()
        }
    } else {
        serde_json::from_value(config.clone()).map_err(|e| {
            CordisError::Configuration(format!("invalid Http config: {e}"))
        })?
    };
    let _ = server_cfg;
    block_on_plugin(
        ctx,
        Http {
            router: build_router(Arc::clone(ctx)),
        },
    )
}

/// Register AuthService and Http loader factories.
/// Overlay is registered by `ares-server` so config watching stays server-owned.
pub fn register_plugins(reg: &PluginRegistry) {
    #[cfg(feature = "postgres")]
    reg.register("AuthService", Arc::new(factory_auth));
    reg.register("Http", Arc::new(factory_http));
}

#[cfg(feature = "inventory")]
inventory::submit! { cordis::CordisInventory { name: "Http" } }
#[cfg(all(feature = "inventory", feature = "postgres"))]
inventory::submit! { cordis::CordisInventory { name: "AuthService" } }

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn http_plugin_serves_health() {
        let ctx = Context::new_root();
        let http = HttpPlugin
            .apply(&ctx, ServerConfig::default())
            .expect("Http::apply");
        let server = axum_test::TestServer::new(http.router.clone()).expect("test server");
        let response = server.get("/health").await;
        response.assert_status_ok();
        response.assert_text("OK");
    }

    #[tokio::test]
    async fn http_plugin_serves_health_context() {
        let ctx = Context::new_root();
        let router = build_router(ctx);
        let server = axum_test::TestServer::new(router).expect("test server");
        let response = server.get("/health/context").await;
        response.assert_status_ok();
    }
}
