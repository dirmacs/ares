//! Loader factories for `ares-store`.
//!
//! The `Store` factory is the single place a database client is constructed.
//! It parses [`crate::DatabaseConfig`], validates it, and dispatches on the
//! declared backend. There is no second route: the old `PostgresClient::new_local`
//! (which discarded its argument and silently used `DATABASE_URL`) is gone, and
//! `DatabaseProvider` — an enum nothing ever selected — is gone with it.
//!
//! Configuration is parsed and validated unconditionally so that a bad config
//! fails at load in every build, not only in builds that happen to include the
//! backend the config names.

#[cfg(any(feature = "postgres", feature = "turso"))]
use std::sync::Arc;

#[cfg(any(feature = "postgres", feature = "turso"))]
use cordis::{CordisError, FiberId};

#[cfg(any(feature = "postgres", feature = "turso"))]
fn block_on_async<F: std::future::Future>(fut: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

#[cfg(any(feature = "postgres", feature = "turso"))]
fn block_on_plugin<S: cordis::Service + 'static>(
    ctx: &Arc<cordis::Context>,
    svc: S,
) -> Result<FiberId, CordisError> {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(ctx.plugin(svc)))
}

/// Register this crate's loader factories on `reg` (manual fallback path).
pub fn register_plugins(reg: &cordis::PluginRegistry) {
    #[cfg(any(feature = "postgres", feature = "turso"))]
    {
        reg.register("Store", Arc::new(factory_store));
    }
    #[cfg(not(any(feature = "postgres", feature = "turso")))]
    {
        let _ = reg;
    }
}

#[cfg(all(feature = "inventory", any(feature = "postgres", feature = "turso")))]
inventory::submit! {
    cordis::CordisPluginFactory { name: "Store", make: factory_store }
}

/// Parse and validate the database configuration carried in a plugin entry.
///
/// Kept separate from construction so the validation rules are testable without
/// a live database, and so both builds apply the same rules.
#[cfg(any(feature = "postgres", feature = "turso"))]
fn parse_database_config(config: &serde_json::Value) -> Result<crate::DatabaseConfig, CordisError> {
    let db: crate::DatabaseConfig =
        if config.is_null() || config.as_object().is_some_and(|obj| obj.is_empty()) {
            crate::DatabaseConfig::default()
        } else {
            serde_json::from_value(config.clone())
                .map_err(|e| CordisError::Configuration(format!("[database] {e}")))?
        };
    db.validate().map_err(CordisError::Configuration)?;
    Ok(db)
}

#[cfg(any(feature = "postgres", feature = "turso"))]
fn factory_store(
    ctx: &Arc<cordis::Context>,
    config: &serde_json::Value,
) -> Result<FiberId, CordisError> {
    let db = parse_database_config(config)?;
    match &db.backend {
        #[cfg(feature = "postgres")]
        crate::DatabaseBackend::Postgres {
            url,
            max_connections,
        } => factory_store_postgres(ctx, url, *max_connections),
        #[cfg(not(feature = "postgres"))]
        crate::DatabaseBackend::Postgres { .. } => Err(CordisError::Configuration(
            "[database] backend = \"postgres\" but this build was compiled without \
             the `postgres` feature; rebuild with --features postgres"
                .to_string(),
        )),

        #[cfg(feature = "turso")]
        crate::DatabaseBackend::Embedded { path } => factory_store_embedded(ctx, path),
        #[cfg(not(feature = "turso"))]
        crate::DatabaseBackend::Embedded { .. } => Err(CordisError::Configuration(
            "[database] backend = \"embedded\" but this build was compiled without \
             the `turso` feature; rebuild with --features turso"
                .to_string(),
        )),
    }
}

#[cfg(feature = "postgres")]
fn factory_store_postgres(
    ctx: &Arc<cordis::Context>,
    url: &str,
    max_connections: Option<u32>,
) -> Result<FiberId, CordisError> {
    let url = crate::postgres::resolve_database_url(Some(url));
    let max_connections = crate::postgres::resolve_max_connections(max_connections);
    tracing::info!(backend = "postgres", max_connections, "Store");
    let pg = block_on_async(crate::PostgresClient::new_remote_with_max_connections(
        url,
        max_connections,
    ))
    .map_err(|e| CordisError::Configuration(e.to_string()))?;
    let pg_arc = Arc::new(pg);
    ctx.provide_arc(pg_arc.clone());

    let tenant = crate::TenantDb::new(pg_arc.clone());
    let fid = block_on_plugin(ctx, tenant)?;

    let pg = ctx
        .get::<crate::TenantDb>()
        .ok_or_else(|| CordisError::Configuration("TenantDb missing after Store factory".into()))?;
    block_on_async(crate::MIGRATOR.run(pg.pool())).map_err(|e| {
        CordisError::Configuration(format!("Failed to run database migrations: {e}"))
    })?;
    tracing::info!("Database migrations applied");

    block_on_async(crate::tenant_agents::seed_default_templates(pg.pool()))
        .map_err(|e| CordisError::Configuration(format!("Failed to seed agent templates: {e}")))?;
    tracing::info!("Agent templates seeded");

    let fleet_secrets = crate::FleetSecrets::new();
    let fleet_store = crate::fleet_provider_secrets::FleetProviderSecretsStore::new(&pg_arc.pool);
    let master = crate::MasterKey::from_env();
    match block_on_async(fleet_store.load_all(master.as_ref())) {
        Ok(providers) => fleet_secrets.store(providers),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load fleet provider secrets");
        }
    }
    block_on_plugin(ctx, fleet_secrets)?;
    Ok(fid)
}

/// Embedded backend: open the file, ensure the schema, provide the client.
///
/// Postgres-only concerns (migrations, tenant seeding, fleet secrets) have no
/// embedded equivalent yet; the embedded schema is created by the client itself.
#[cfg(feature = "turso")]
fn factory_store_embedded(ctx: &Arc<cordis::Context>, path: &str) -> Result<FiberId, CordisError> {
    tracing::info!(backend = "embedded", path, "Store");
    let client = block_on_async(crate::TursoClient::new_local(path))
        .map_err(|e| CordisError::Configuration(format!("embedded store: {e}")))?;
    let fid = block_on_plugin(ctx, client.clone())?;
    ctx.provide_arc(Arc::new(client));
    Ok(fid)
}

#[cfg(all(test, any(feature = "postgres", feature = "turso")))]
mod tests {
    use super::{parse_database_config, register_plugins};
    use cordis::PluginRegistry;

    #[test]
    fn register_plugins_store_key() {
        let reg = PluginRegistry::new();
        register_plugins(&reg);
        assert!(reg.get("Store").is_some());
    }

    #[test]
    fn empty_entry_yields_the_default_backend() {
        let cfg = parse_database_config(&serde_json::json!({})).expect("default config");
        assert_eq!(cfg.backend, crate::DatabaseBackend::default());
    }

    #[test]
    fn a_filesystem_path_in_a_postgres_url_is_rejected_at_load() {
        let err = parse_database_config(&serde_json::json!({
            "backend": "postgres",
            "url": "./data/ares.db"
        }))
        .expect_err("must reject a path used as a url");
        let msg = err.to_string();
        assert!(msg.contains("postgres://"), "got {msg}");
        assert!(msg.contains("embedded"), "got {msg}");
    }

    #[test]
    fn an_unknown_backend_is_rejected_at_load() {
        let err = parse_database_config(&serde_json::json!({
            "backend": "sqlite",
            "path": "./x.db"
        }))
        .expect_err("must not silently default");
        assert!(err.to_string().contains("[database]"));
    }
}
