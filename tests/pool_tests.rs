//! Integration tests for LLM Client Pooling (DIR-44)
//!
//! These tests verify the connection pooling functionality for LLM clients.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

// Mock imports for testing
use async_trait::async_trait;

/// A mock LLM client for testing pool behavior
#[derive(Clone)]
struct MockLLMClient {
    id: usize,
    model: String,
    call_count: Arc<AtomicUsize>,
}

impl MockLLMClient {
    fn new(id: usize) -> Self {
        Self {
            id,
            model: format!("mock-model-{}", id),
            call_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn with_shared_counter(id: usize, counter: Arc<AtomicUsize>) -> Self {
        Self {
            id,
            model: format!("mock-model-{}", id),
            call_count: counter,
        }
    }
}

#[cfg(test)]
mod pool_config_tests {
    use ares::llm::pool::PoolConfig;
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
    use ares::llm::pool::{ClientPool, ClientPoolBuilder, PoolConfig};

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
#[cfg(feature = "ollama")]
mod pool_provider_tests {
    use ares::llm::client::{ModelParams, Provider};
    use ares::llm::pool::{ClientPool, ClientPoolBuilder, PoolConfig};

    fn create_test_provider() -> Provider {
        Provider::Ollama {
            base_url: "http://localhost:11434".to_string(),
            model: "test-model".to_string(),
            params: ModelParams::default(),
        }
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
    use ares::llm::pool::{ClientPool, PoolConfig};
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
#[cfg(feature = "ollama")]
mod pool_lifecycle_tests {
    use ares::llm::client::{ModelParams, Provider};
    use ares::llm::pool::{ClientPool, PoolConfig};
    use std::sync::Arc;
    use std::time::Duration;

    fn create_test_provider() -> Provider {
        Provider::Ollama {
            base_url: "http://localhost:11434".to_string(),
            model: "test-model".to_string(),
            params: ModelParams::default(),
        }
    }

    #[tokio::test]
    async fn test_cleanup_task_respects_shutdown() {
        let pool = Arc::new(ClientPool::new(
            PoolConfig::default().with_idle_timeout(Duration::from_millis(10)),
        ));

        let handle = pool.start_cleanup_task();

        // Give it a moment to start
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Shutdown should cause cleanup task to exit
        pool.shutdown();

        // Task should complete relatively quickly
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
    use ares::llm::pool::{ClientPool, PoolStats};

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
    use ares::llm::pool::{ClientPoolBuilder, PoolConfig};
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

    #[cfg(feature = "ollama")]
    #[test]
    fn test_builder_with_multiple_providers() {
        use ares::llm::client::{ModelParams, Provider};

        let pool = ClientPoolBuilder::new()
            .provider(
                "ollama1",
                Provider::Ollama {
                    base_url: "http://localhost:11434".to_string(),
                    model: "model1".to_string(),
                    params: ModelParams::default(),
                },
            )
            .provider(
                "ollama2",
                Provider::Ollama {
                    base_url: "http://localhost:11435".to_string(),
                    model: "model2".to_string(),
                    params: ModelParams::default(),
                },
            )
            .build();

        assert!(pool.has_provider("ollama1"));
        assert!(pool.has_provider("ollama2"));
        assert_eq!(pool.provider_names().len(), 2);
    }
}
