//! Loader factories for `ares-store`.
//!
//! With `postgres`, `register_plugins` installs the `Store` factory. Without
//! that feature the function is a no-op so other crates can always call it.

#[cfg(feature = "postgres")]
use std::sync::Arc;

#[cfg(feature = "postgres")]
use cordis::{CordisError, FiberId};

#[cfg(feature = "postgres")]
fn block_on_async<F: std::future::Future>(fut: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

#[cfg(feature = "postgres")]
fn block_on_plugin<S: cordis::Service + 'static>(
    ctx: &Arc<cordis::Context>,
    svc: S,
) -> Result<FiberId, CordisError> {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(ctx.plugin(svc)))
}

/// Register this crate's loader factories on `reg`.
pub fn register_plugins(reg: &cordis::PluginRegistry) {
    #[cfg(feature = "postgres")]
    {
        reg.register("Store", Arc::new(factory_store));
    }
    #[cfg(not(feature = "postgres"))]
    {
        let _ = reg;
    }
}

#[cfg(feature = "postgres")]
fn factory_store(
    ctx: &Arc<cordis::Context>,
    config: &serde_json::Value,
) -> Result<FiberId, CordisError> {
    let db: crate::DatabaseConfig =
        if config.is_null() || config.as_object().is_some_and(|obj| obj.is_empty()) {
            crate::DatabaseConfig::default()
        } else {
            serde_json::from_value(config.clone())
                .map_err(|e| CordisError::Configuration(e.to_string()))?
        };

    let url = crate::postgres::resolve_database_url(Some(&db.url));
    let pg = block_on_async(crate::PostgresClient::new_remote(url, String::new()))
        .map_err(|e| CordisError::Configuration(e.to_string()))?;
    let pg_arc = Arc::new(pg);
    ctx.provide_arc(pg_arc.clone());

    let tenant = crate::TenantDb::new(pg_arc.clone());
    let fid = block_on_plugin(ctx, tenant)?;

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

#[cfg(test)]
mod tests {
    use super::register_plugins;
    use cordis::PluginRegistry;

    #[test]
    fn register_plugins_store_key() {
        let reg = PluginRegistry::new();
        register_plugins(&reg);
        #[cfg(feature = "postgres")]
        assert!(reg.get("Store").is_some());
        #[cfg(not(feature = "postgres"))]
        assert!(reg.get("Store").is_none());
    }
}
