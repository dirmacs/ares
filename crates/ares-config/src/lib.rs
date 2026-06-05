pub mod nvidia_catalog;
pub mod toml_config;
pub mod toon_config;
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
}
