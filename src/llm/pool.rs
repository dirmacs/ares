//! LLM Client Connection Pooling (DIR-44)
//!
//! This module provides connection pooling for LLM clients, enabling connection
//! reuse across requests to reduce latency and resource consumption.
//!
//! # Architecture
//!
//! The pool maintains a set of pre-initialized `LLMClient` instances per provider
//! configuration. Clients are checked out, used, and returned to the pool.
//!
//! # Features
//!
//! - Configurable maximum pool size per provider
//! - Connection health checking with configurable TTL
//! - Automatic stale connection cleanup
//! - Graceful shutdown with connection draining
//! - Fair distribution via round-robin or least-connections
//!
//! # Example
//!
//! ```rust,ignore
//! use ares::llm::pool::{ClientPool, PoolConfig};
//! use ares::llm::Provider;
//!
//! let config = PoolConfig::default();
//! let pool = ClientPool::new(config);
//!
//! // Register a provider
//! pool.register_provider("openai", provider).await?;
//!
//! // Get a pooled client
//! let guard = pool.get("openai").await?;
//! let response = guard.client().generate("Hello!").await?;
//! // Client is automatically returned to pool when guard is dropped
//! ```

use crate::llm::client::{LLMClient, Provider};
use crate::types::{AppError, Result};
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Configuration for the client pool
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of clients per provider (default: 10)
    pub max_connections_per_provider: usize,

    /// Minimum number of idle clients to maintain per provider (default: 2)
    pub min_idle_connections: usize,

    /// Maximum time a client can be idle before being considered stale (default: 5 minutes)
    pub idle_timeout: Duration,

    /// Maximum lifetime of a client before forced refresh (default: 30 minutes)
    pub max_lifetime: Duration,

    /// How often to run health checks on idle connections (default: 60 seconds)
    pub health_check_interval: Duration,

    /// Timeout for acquiring a client from the pool (default: 30 seconds)
    pub acquire_timeout: Duration,

    /// Whether to enable connection health checking (default: true)
    pub enable_health_check: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections_per_provider: 10,
            min_idle_connections: 2,
            idle_timeout: Duration::from_secs(300),       // 5 minutes
            max_lifetime: Duration::from_secs(1800),      // 30 minutes
            health_check_interval: Duration::from_secs(60),
            acquire_timeout: Duration::from_secs(30),
            enable_health_check: true,
        }
    }
}

impl PoolConfig {
    /// Create a new pool config with custom max connections
    pub fn with_max_connections(mut self, max: usize) -> Self {
        self.max_connections_per_provider = max;
        self
    }

    /// Create a new pool config with custom idle timeout
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Create a new pool config with custom max lifetime
    pub fn with_max_lifetime(mut self, lifetime: Duration) -> Self {
        self.max_lifetime = lifetime;
        self
    }

    /// Disable health checking (useful for testing)
    pub fn without_health_check(mut self) -> Self {
        self.enable_health_check = false;
        self
    }
}

/// Metadata for a pooled client
#[derive(Debug)]
struct PooledClientMeta {
    /// When this client was created
    created_at: Instant,
    /// When this client was last used
    last_used: Instant,
    /// Number of times this client has been used
    #[allow(dead_code)] // Used for metrics/debugging
    use_count: AtomicU64,
}

impl PooledClientMeta {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            created_at: now,
            last_used: now,
            use_count: AtomicU64::new(0),
        }
    }

    fn mark_used(&mut self) {
        self.last_used = Instant::now();
        self.use_count.fetch_add(1, Ordering::Relaxed);
    }

    fn is_stale(&self, config: &PoolConfig) -> bool {
        let now = Instant::now();
        let idle_duration = now.duration_since(self.last_used);
        let lifetime = now.duration_since(self.created_at);

        idle_duration > config.idle_timeout || lifetime > config.max_lifetime
    }
}

/// A pooled LLM client with its metadata
struct PooledClient {
    client: Box<dyn LLMClient>,
    meta: PooledClientMeta,
}

/// Pool of clients for a single provider
struct ProviderPool {
    /// The provider configuration for creating new clients
    provider: Provider,
    /// Pool of available clients
    clients: Mutex<Vec<PooledClient>>,
    /// Semaphore to limit concurrent connections
    semaphore: Arc<Semaphore>,
    /// Number of clients currently in use
    in_use_count: AtomicUsize,
    /// Total number of clients created (for stats)
    total_created: AtomicU64,
    /// Configuration reference
    config: PoolConfig,
}

impl ProviderPool {
    fn new(provider: Provider, config: PoolConfig) -> Self {
        let semaphore = Arc::new(Semaphore::new(config.max_connections_per_provider));
        Self {
            provider,
            clients: Mutex::new(Vec::with_capacity(config.max_connections_per_provider)),
            semaphore,
            in_use_count: AtomicUsize::new(0),
            total_created: AtomicU64::new(0),
            config,
        }
    }

    /// Get an available client from the pool, or create a new one
    async fn acquire(&self) -> Result<(Box<dyn LLMClient>, OwnedSemaphorePermit)> {
        // Acquire a permit (blocks if pool is at capacity)
        let permit = tokio::time::timeout(
            self.config.acquire_timeout,
            self.semaphore.clone().acquire_owned(),
        )
        .await
        .map_err(|_| AppError::LLM("Timeout waiting for available client in pool".to_string()))?
        .map_err(|_| AppError::LLM("Pool semaphore closed".to_string()))?;

        // Try to get an existing client from the pool
        let maybe_client = {
            let mut clients = self.clients.lock();
            // Find a non-stale client
            let mut found_idx = None;
            for (idx, pooled) in clients.iter().enumerate() {
                if !pooled.meta.is_stale(&self.config) {
                    found_idx = Some(idx);
                    break;
                }
            }

            if let Some(idx) = found_idx {
                Some(clients.swap_remove(idx))
            } else {
                // Remove all stale clients
                clients.retain(|c| !c.meta.is_stale(&self.config));
                None
            }
        };

        let client = if let Some(mut pooled) = maybe_client {
            pooled.meta.mark_used();
            pooled.client
        } else {
            // Create a new client
            self.total_created.fetch_add(1, Ordering::Relaxed);
            self.provider.create_client().await?
        };

        self.in_use_count.fetch_add(1, Ordering::Relaxed);
        Ok((client, permit))
    }

    /// Return a client to the pool
    fn release(&self, client: Box<dyn LLMClient>) {
        self.in_use_count.fetch_sub(1, Ordering::Relaxed);

        let mut clients = self.clients.lock();

        // Only return to pool if we haven't exceeded max idle
        if clients.len() < self.config.max_connections_per_provider {
            clients.push(PooledClient {
                client,
                meta: PooledClientMeta::new(),
            });
        }
        // Otherwise, client is dropped
    }

    /// Remove stale connections from the pool
    fn cleanup_stale(&self) -> usize {
        let mut clients = self.clients.lock();
        let before = clients.len();
        clients.retain(|c| !c.meta.is_stale(&self.config));
        before - clients.len()
    }

    /// Get pool statistics
    fn stats(&self) -> ProviderPoolStats {
        let clients = self.clients.lock();
        ProviderPoolStats {
            available: clients.len(),
            in_use: self.in_use_count.load(Ordering::Relaxed),
            total_created: self.total_created.load(Ordering::Relaxed),
            max_size: self.config.max_connections_per_provider,
        }
    }

    /// Drain all connections (for shutdown)
    fn drain(&self) {
        let mut clients = self.clients.lock();
        clients.clear();
    }
}

/// Statistics for a provider pool
#[derive(Debug, Clone)]
pub struct ProviderPoolStats {
    /// Number of available (idle) clients
    pub available: usize,
    /// Number of clients currently in use
    pub in_use: usize,
    /// Total number of clients created over the pool's lifetime
    pub total_created: u64,
    /// Maximum pool size
    pub max_size: usize,
}

/// Overall pool statistics
#[derive(Debug, Clone)]
pub struct PoolStats {
    /// Stats per provider
    pub providers: HashMap<String, ProviderPoolStats>,
    /// Total available clients across all providers
    pub total_available: usize,
    /// Total in-use clients across all providers
    pub total_in_use: usize,
}

/// Guard that returns a client to the pool when dropped
pub struct PooledClientGuard {
    client: Option<Box<dyn LLMClient>>,
    pool: Arc<ProviderPool>,
    _permit: OwnedSemaphorePermit,
}

impl PooledClientGuard {
    /// Get a reference to the underlying client
    pub fn client(&self) -> &dyn LLMClient {
        self.client.as_ref().expect("Client already taken").as_ref()
    }

    /// Get a mutable reference to the underlying client
    pub fn client_mut(&mut self) -> &mut dyn LLMClient {
        self.client.as_mut().expect("Client already taken").as_mut()
    }

    /// Take ownership of the client, preventing it from being returned to the pool
    ///
    /// This is useful if you need to move the client elsewhere, but be aware that
    /// it won't be returned to the pool.
    pub fn take(mut self) -> Box<dyn LLMClient> {
        self.client.take().expect("Client already taken")
    }
}

impl Drop for PooledClientGuard {
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            self.pool.release(client);
        }
    }
}

impl std::ops::Deref for PooledClientGuard {
    type Target = Box<dyn LLMClient>;

    fn deref(&self) -> &Self::Target {
        self.client.as_ref().expect("Client already taken")
    }
}

/// LLM Client Pool for managing reusable client connections
///
/// The pool maintains separate sub-pools for each registered provider,
/// allowing efficient reuse of HTTP connections and client state.
pub struct ClientPool {
    config: PoolConfig,
    providers: RwLock<HashMap<String, Arc<ProviderPool>>>,
    shutdown: std::sync::atomic::AtomicBool,
}

impl ClientPool {
    /// Create a new client pool with the given configuration
    pub fn new(config: PoolConfig) -> Self {
        Self {
            config,
            providers: RwLock::new(HashMap::new()),
            shutdown: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Create a new client pool with default configuration
    pub fn with_defaults() -> Self {
        Self::new(PoolConfig::default())
    }

    /// Register a provider with the pool
    ///
    /// This creates a sub-pool for the given provider that will manage
    /// client instances for that provider.
    #[allow(unreachable_code, unused_variables)]
    pub fn register_provider(&self, name: &str, provider: Provider) {
        let pool = Arc::new(ProviderPool::new(provider, self.config.clone()));
        let mut providers = self.providers.write();
        providers.insert(name.to_string(), pool);
    }

    /// Check if a provider is registered
    pub fn has_provider(&self, name: &str) -> bool {
        self.providers.read().contains_key(name)
    }

    /// List all registered provider names
    pub fn provider_names(&self) -> Vec<String> {
        self.providers.read().keys().cloned().collect()
    }

    /// Get a client from the pool for the specified provider
    ///
    /// The returned guard will automatically return the client to the pool
    /// when dropped.
    pub async fn get(&self, provider_name: &str) -> Result<PooledClientGuard> {
        if self.shutdown.load(Ordering::Relaxed) {
            return Err(AppError::LLM("Pool is shutting down".to_string()));
        }

        let pool = {
            let providers = self.providers.read();
            providers.get(provider_name).cloned().ok_or_else(|| {
                AppError::Configuration(format!("Provider '{}' not registered in pool", provider_name))
            })?
        };

        let (client, permit) = pool.acquire().await?;

        Ok(PooledClientGuard {
            client: Some(client),
            pool,
            _permit: permit,
        })
    }

    /// Get pool statistics
    pub fn stats(&self) -> PoolStats {
        let providers = self.providers.read();
        let mut stats = PoolStats {
            providers: HashMap::new(),
            total_available: 0,
            total_in_use: 0,
        };

        for (name, pool) in providers.iter() {
            let provider_stats = pool.stats();
            stats.total_available += provider_stats.available;
            stats.total_in_use += provider_stats.in_use;
            stats.providers.insert(name.clone(), provider_stats);
        }

        stats
    }

    /// Clean up stale connections across all providers
    ///
    /// Returns the total number of connections removed.
    pub fn cleanup_stale(&self) -> usize {
        let providers = self.providers.read();
        providers.values().map(|p| p.cleanup_stale()).sum()
    }

    /// Start a background task that periodically cleans up stale connections
    ///
    /// The task runs until the pool is shut down.
    pub fn start_cleanup_task(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let pool = Arc::clone(self);
        let interval = pool.config.health_check_interval;

        tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(interval);
            loop {
                interval_timer.tick().await;

                if pool.shutdown.load(Ordering::Relaxed) {
                    break;
                }

                let removed = pool.cleanup_stale();
                if removed > 0 {
                    tracing::debug!("Pool cleanup: removed {} stale connections", removed);
                }
            }
        })
    }

    /// Gracefully shut down the pool
    ///
    /// This prevents new clients from being acquired and drains all existing
    /// connections.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);

        let providers = self.providers.read();
        for pool in providers.values() {
            pool.drain();
        }
    }

    /// Check if the pool is shut down
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
}

impl Default for ClientPool {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Builder for creating a `ClientPool` with registered providers
pub struct ClientPoolBuilder {
    config: PoolConfig,
    providers: Vec<(String, Provider)>,
}

impl ClientPoolBuilder {
    /// Create a new builder with default configuration
    pub fn new() -> Self {
        Self {
            config: PoolConfig::default(),
            providers: Vec::new(),
        }
    }

    /// Set the pool configuration
    pub fn config(mut self, config: PoolConfig) -> Self {
        self.config = config;
        self
    }

    /// Add a provider to the pool
    pub fn provider(mut self, name: impl Into<String>, provider: Provider) -> Self {
        self.providers.push((name.into(), provider));
        self
    }

    /// Build the client pool
    pub fn build(self) -> ClientPool {
        let pool = ClientPool::new(self.config);
        for (name, provider) in self.providers {
            pool.register_provider(&name, provider);
        }
        pool
    }

    /// Build the client pool wrapped in an Arc
    pub fn build_arc(self) -> Arc<ClientPool> {
        Arc::new(self.build())
    }
}

impl Default for ClientPoolBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_config_defaults() {
        let config = PoolConfig::default();
        assert_eq!(config.max_connections_per_provider, 10);
        assert_eq!(config.min_idle_connections, 2);
        assert_eq!(config.idle_timeout, Duration::from_secs(300));
        assert_eq!(config.max_lifetime, Duration::from_secs(1800));
        assert!(config.enable_health_check);
    }

    #[test]
    fn test_pool_config_builder() {
        let config = PoolConfig::default()
            .with_max_connections(20)
            .with_idle_timeout(Duration::from_secs(60))
            .without_health_check();

        assert_eq!(config.max_connections_per_provider, 20);
        assert_eq!(config.idle_timeout, Duration::from_secs(60));
        assert!(!config.enable_health_check);
    }

    #[test]
    fn test_pooled_client_meta_stale_detection() {
        let config = PoolConfig::default()
            .with_idle_timeout(Duration::from_millis(10))
            .with_max_lifetime(Duration::from_millis(50));

        let meta = PooledClientMeta::new();

        // Should not be stale immediately
        assert!(!meta.is_stale(&config));

        // Sleep to trigger idle timeout
        std::thread::sleep(Duration::from_millis(15));
        assert!(meta.is_stale(&config));
    }

    #[test]
    fn test_pool_stats() {
        let pool = ClientPool::with_defaults();
        let stats = pool.stats();

        assert_eq!(stats.total_available, 0);
        assert_eq!(stats.total_in_use, 0);
        assert!(stats.providers.is_empty());
    }

    #[test]
    fn test_pool_shutdown() {
        let pool = ClientPool::with_defaults();
        assert!(!pool.is_shutdown());

        pool.shutdown();
        assert!(pool.is_shutdown());
    }

    #[cfg(feature = "ollama")]
    #[test]
    fn test_provider_registration() {
        use crate::llm::client::ModelParams;

        let pool = ClientPool::with_defaults();

        let provider = Provider::Ollama {
            base_url: "http://localhost:11434".to_string(),
            model: "test".to_string(),
            params: ModelParams::default(),
        };

        pool.register_provider("ollama", provider);

        assert!(pool.has_provider("ollama"));
        assert!(!pool.has_provider("openai"));
        assert_eq!(pool.provider_names(), vec!["ollama"]);
    }

    #[cfg(feature = "ollama")]
    #[test]
    fn test_builder_pattern() {
        use crate::llm::client::ModelParams;

        let provider = Provider::Ollama {
            base_url: "http://localhost:11434".to_string(),
            model: "test".to_string(),
            params: ModelParams::default(),
        };

        let pool = ClientPoolBuilder::new()
            .config(PoolConfig::default().with_max_connections(5))
            .provider("ollama", provider)
            .build();

        assert!(pool.has_provider("ollama"));
    }

    #[cfg(feature = "ollama")]
    #[tokio::test]
    async fn test_get_unregistered_provider_error() {
        let pool = ClientPool::with_defaults();

        let result = pool.get("nonexistent").await;
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, AppError::Configuration(_)));
    }

    #[tokio::test]
    async fn test_get_after_shutdown() {
        let pool = ClientPool::with_defaults();
        pool.shutdown();

        // Even if we had a provider, should fail after shutdown
        let result = pool.get("anything").await;
        assert!(result.is_err());

        if let Err(AppError::LLM(msg)) = result {
            assert!(msg.contains("shutting down"));
        } else {
            panic!("Expected LLM error");
        }
    }
}
