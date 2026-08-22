pub mod domains;
pub mod fleet_secrets;
pub mod nvidia_catalog;
pub mod toml_config;
pub mod toon_config;
pub use fleet_secrets::{
    decrypt_api_key, encrypt_api_key, last_n_visible, EncryptedPayload, FleetSecrets,
    FleetSecretsError, MasterKey, ProviderOverride,
};
pub use nvidia_catalog::{NvidiaConfig, NvidiaCatalogCache, CatalogEntry};
pub use toml_config::AresConfigManager;
pub use toon_config::DynamicConfigManager;

#[cfg(test)]
mod tests {
    use super::{AresConfigManager, DynamicConfigManager};

    #[test]
    fn config_manager_types_are_reexported() {
        fn assert_send<T: Send>() {}
        assert_send::<AresConfigManager>();
        assert_send::<DynamicConfigManager>();
    }

    #[test]
    fn fleet_secrets_is_send_sync() {
        fn assert_send<T: Send + Sync>() {}
        assert_send::<super::fleet_secrets::FleetSecrets>();
        assert_send::<super::fleet_secrets::MasterKey>();
    }
}
