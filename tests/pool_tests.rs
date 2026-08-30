//! Integration tests for LLM Client Pooling (DIR-44)
//!
//! These tests verify the connection pooling functionality for LLM clients.


fn ollama_provider(base_url: &str, model: &str) -> ares_llm::Provider {
    use ares_llm::{AdapterKind, GenaiProvider, Provider};
    Provider::Genai(GenaiProvider {
        kind: AdapterKind::Ollama,
        api_key: None,
        endpoint: Some(base_url.to_string()),
        model: model.to_string(),
        params: Default::default(),
        headers: Default::default(),
        region: None,
        vertex_project: None,
        vertex_location: None,
        custom_index: None,
    })
}

#[cfg(test)]
mod pool_config_tests {
    use ares_llm::pool::PoolConfig;
    use std::time::Duration;

    #[test]
    fn test_default_config() {
        let config = PoolConfig::default();

        assert_eq!(config.max_connections_per_provider, 10);
        assert_eq!(config.min_idle_connections, 2);
        assert_eq!(config.idle_timeout, Duration::from_secs(300));
        assert_eq!(config.max_lifetime, Duration::from_secs(1800));
        assert_eq!(config.health_check_interval, Duration::from_secs(60));
        assert_eq!(config.acquire_timeout, Duration::from_secs(30));
        assert!(config.enable_health_check);
    }

    #[test]
    fn test_config_builder_chaining() {
        let config = PoolConfig::default()
            .with_max_connections(5)
            .with_idle_timeout(Duration::from_secs(60))
            .with_max_lifetime(Duration::from_secs(600))
            .without_health_check();

        assert_eq!(config.max_connections_per_provider, 5);
        assert_eq!(config.idle_timeout, Duration::from_secs(60));
        assert_eq!(config.max_lifetime, Duration::from_secs(600));
        assert!(!config.enable_health_check);
    }

    #[test]
    fn test_config_reasonable_defaults_for_production() {
        let config = PoolConfig::default();

        // Should have reasonable defaults for production use
        assert!(config.max_connections_per_provider >= 5);
        assert!(config.max_connections_per_provider <= 50);
        assert!(config.idle_timeout >= Duration::from_secs(60));
        assert!(config.max_lifetime >= Duration::from_secs(300));
    }
}

#[cfg(test)]
mod pool_basic_tests {
    use ares_llm::pool::{ClientPool, ClientPoolBuilder};

    #[test]
    fn test_pool_creation_with_defaults() {
        let pool = ClientPool::with_defaults();
        assert!(!pool.is_shutdown());
        assert!(pool.provider_names().is_empty());
    }

    #[test]
    fn test_pool_creation_with_config() {
        let config = PoolConfig::default().with_max_connections(5);
        let pool = ClientPool::new(config);
        assert!(!pool.is_shutdown());
    }

    #[test]
    fn test_pool_builder() {
        let pool = ClientPoolBuilder::new()
            .config(PoolConfig::default().with_max_connections(3))
            .build();

        assert!(!pool.is_shutdown());
    }

    #[test]
    fn test_pool_shutdown() {
        let pool = ClientPool::with_defaults();
        assert!(!pool.is_shutdown());

        pool.shutdown();
        assert!(pool.is_shutdown());
    }

    #[test]
    fn test_pool_stats_empty() {
        let pool = ClientPool::with_defaults();
        let stats = pool.stats();

        assert_eq!(stats.total_available, 0);
        assert_eq!(stats.total_in_use, 0);
        assert!(stats.providers.is_empty());
    }
}

#[cfg(test)]
mod pool_provider_tests {
    use ares_llm::client::Provider;
    use ares_llm::pool::{ClientPool, ClientPoolBuilder};

    fn create_test_provider() -> Provider {
        crate::ollama_provider("http://localhost:11434", "test-model")
    }

    #[test]
    fn test_register_provider() {
        let pool = ClientPool::with_defaults();
        let provider = create_test_provider();

        pool.register_provider("ollama", provider);

        assert!(pool.has_provider("ollama"));
        assert!(!pool.has_provider("openai"));
    }

    #[test]
    fn test_register_multiple_providers() {
        let pool = ClientPool::with_defaults();

        pool.register_provider("ollama1", create_test_provider());
        pool.register_provider("ollama2", create_test_provider());
        pool.register_provider("ollama3", create_test_provider());

        assert_eq!(pool.provider_names().len(), 3);
        assert!(pool.has_provider("ollama1"));
        assert!(pool.has_provider("ollama2"));
        assert!(pool.has_provider("ollama3"));
    }

    #[test]
    fn test_builder_with_providers() {
        let pool = ClientPoolBuilder::new()
            .provider("ollama", create_test_provider())
            .build();

        assert!(pool.has_provider("ollama"));
    }

    #[test]
    fn test_stats_with_providers() {
        let pool = ClientPool::with_defaults();
        pool.register_provider("ollama", create_test_provider());

        let stats = pool.stats();

        assert!(stats.providers.contains_key("ollama"));
        let ollama_stats = &stats.providers["ollama"];
        assert_eq!(ollama_stats.available, 0);
        assert_eq!(ollama_stats.in_use, 0);
        assert_eq!(ollama_stats.total_created, 0);
    }

    #[tokio::test]
    async fn test_get_unregistered_provider() {
        let pool = ClientPool::with_defaults();

        let result = pool.get("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_after_shutdown() {
        let pool = ClientPool::with_defaults();
        pool.register_provider("ollama", create_test_provider());
        pool.shutdown();

        let result = pool.get("ollama").await;
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod pool_concurrency_tests {
    use ares_llm::pool::{ClientPool, PoolConfig};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn test_pool_is_thread_safe() {
        let pool = Arc::new(ClientPool::with_defaults());

        // Spawn multiple tasks that access the pool concurrently
        let mut handles = vec![];

        for _ in 0..10 {
            let pool = Arc::clone(&pool);
            handles.push(tokio::spawn(async move {
                // Just verify we can access the pool from multiple tasks
                let _ = pool.stats();
                let _ = pool.provider_names();
                let _ = pool.has_provider("test");
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_concurrent_stats_access() {
        let pool = Arc::new(ClientPool::with_defaults());

        let handles: Vec<_> = (0..100)
            .map(|_| {
                let pool = Arc::clone(&pool);
                tokio::spawn(async move { pool.stats() })
            })
            .collect();

        for handle in handles {
            let stats = handle.await.unwrap();
            assert_eq!(stats.total_available, 0);
        }
    }

    #[tokio::test]
    async fn test_cleanup_stale_empty_pool() {
        let pool = ClientPool::new(
            PoolConfig::default()
                .with_idle_timeout(Duration::from_millis(1))
                .with_max_lifetime(Duration::from_millis(1)),
        );

        // Should not panic on empty pool
        let removed = pool.cleanup_stale();
        assert_eq!(removed, 0);
    }
}

#[cfg(test)]
mod pool_lifecycle_tests {
    use ares_llm::client::Provider;
    use ares_llm::pool::{ClientPool, PoolConfig};
    use std::sync::Arc;
    use std::time::Duration;

    fn create_test_provider() -> Provider {
        crate::ollama_provider("http://localhost:11434", "test-model")
    }

    #[tokio::test]
    async fn test_cleanup_task_respects_shutdown() {
        let mut config = PoolConfig::default().with_idle_timeout(Duration::from_millis(100));
        // Override health_check_interval so the cleanup loop ticks fast enough
        // to observe the shutdown flag within the test timeout
        config.health_check_interval = Duration::from_millis(50);

        let pool = Arc::new(ClientPool::new(config));

        let handle = pool.start_cleanup_task();

        // Give the cleanup task time to start and tick at least once
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Shutdown should cause cleanup task to exit on next tick
        pool.shutdown();

        // Task should complete within a few ticks
        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_pool_drain_on_shutdown() {
        let pool = ClientPool::with_defaults();
        pool.register_provider("ollama", create_test_provider());

        // Verify provider is registered
        assert!(pool.has_provider("ollama"));

        // Shutdown drains connections
        pool.shutdown();

        // Pool should be shutdown
        assert!(pool.is_shutdown());
    }
}

#[cfg(test)]
mod pool_stats_tests {
    use ares_llm::pool::{ClientPool, PoolStats};

    #[test]
    fn test_pool_stats_structure() {
        let pool = ClientPool::with_defaults();
        let stats: PoolStats = pool.stats();

        // Verify the stats structure
        assert!(stats.providers.is_empty());
        assert_eq!(stats.total_available, 0);
        assert_eq!(stats.total_in_use, 0);
    }

    #[test]
    fn test_pool_stats_debug() {
        let pool = ClientPool::with_defaults();
        let stats = pool.stats();

        // Should be debuggable
        let debug_str = format!("{:?}", stats);
        assert!(debug_str.contains("PoolStats"));
    }

    #[test]
    fn test_pool_stats_clone() {
        let pool = ClientPool::with_defaults();
        let stats = pool.stats();

        // Should be cloneable
        let cloned = stats.clone();
        assert_eq!(cloned.total_available, stats.total_available);
        assert_eq!(cloned.total_in_use, stats.total_in_use);
    }
}

#[cfg(test)]
mod pool_builder_tests {
    use ares_llm::pool::{ClientPoolBuilder, PoolConfig};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn test_builder_default() {
        let builder = ClientPoolBuilder::default();
        let pool = builder.build();

        assert!(!pool.is_shutdown());
    }

    #[test]
    fn test_builder_new() {
        let builder = ClientPoolBuilder::new();
        let pool = builder.build();

        assert!(!pool.is_shutdown());
    }

    #[test]
    fn test_builder_custom_config() {
        let config = PoolConfig::default()
            .with_max_connections(3)
            .with_idle_timeout(Duration::from_secs(30));

        let pool = ClientPoolBuilder::new().config(config).build();

        assert!(!pool.is_shutdown());
    }

    #[test]
    fn test_builder_build_arc() {
        let pool: Arc<_> = ClientPoolBuilder::new().build_arc();

        assert!(!pool.is_shutdown());
    }

    #[test]
    fn test_builder_with_multiple_providers() {
        let pool = ClientPoolBuilder::new()
            .provider(
                "ollama1",
                crate::ollama_provider("http://localhost:11434", "model1"),
            )
            .provider(
                "ollama2",
                crate::ollama_provider("http://localhost:11435", "model2"),
            )
            .build();

        assert!(pool.has_provider("ollama1"));
        assert!(pool.has_provider("ollama2"));
        assert_eq!(pool.provider_names().len(), 2);
    }
}
