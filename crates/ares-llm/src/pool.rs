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

use crate::client::{LLMClient, Provider};
use crate::governor::{GovernorConfig, ProviderGovernor};
use ares_types::types::{AppError, Result};
#[cfg(test)]
use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Pool-specific errors for borrow/return operations (R42).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PoolError {
    #[serde(rename = "pool_exhausted")]
    PoolExhausted { max: usize },
    #[serde(rename = "timeout")]
    Timeout { timeout_ms: u64 },
    #[serde(rename = "invalid_client")]
    InvalidClient { reason: String },
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PoolExhausted { max } => write!(f, "pool exhausted (max={max})"),
            Self::Timeout { timeout_ms } => write!(f, "pool acquire timeout after {timeout_ms}ms"),
            Self::InvalidClient { reason } => write!(f, "invalid pooled client: {reason}"),
        }
    }
}

impl std::error::Error for PoolError {}

impl From<PoolError> for AppError {
    fn from(err: PoolError) -> Self {
        AppError::LLM(err.to_string())
    }
}

pub(crate) mod duration_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(value: &Duration, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(value.as_secs())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> std::result::Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Duration::from_secs(u64::deserialize(deserializer)?))
    }
}

/// Configuration for the client pool
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PoolConfig {
    /// Maximum number of in-flight dispatches admitted per provider.
    ///
    /// `None` (default) keeps admission unlimited — governed wrappers are
    /// never installed and behavior is unchanged. See [`ProviderGovernor`]
    /// for the WHO-vs-HOW-MUCH throttle split: this caps how much load one
    /// backend absorbs; tenant-level quotas decide who may call at all.
    #[serde(default)]
    pub max_in_flight: Option<usize>,

    /// Maximum time a dispatch waits for an in-flight slot before failing
    /// closed (only relevant when `max_in_flight` is set).
    #[serde(default = "default_acquire_timeout_secs", with = "duration_secs")]
    pub governor_acquire_timeout: Duration,

    /// Maximum number of clients per provider (default: 10)
    pub max_connections_per_provider: usize,

    /// Minimum number of idle clients to maintain per provider (default: 2)
    pub min_idle_connections: usize,

    /// Maximum time a client can be idle before being considered stale (default: 5 minutes)
    #[serde(with = "duration_secs")]
    pub idle_timeout: Duration,

    /// Maximum lifetime of a client before forced refresh (default: 30 minutes)
    #[serde(with = "duration_secs")]
    pub max_lifetime: Duration,

    /// How often to run health checks on idle connections (default: 60 seconds)
    #[serde(with = "duration_secs")]
    pub health_check_interval: Duration,

    /// Timeout for acquiring a client from the pool (default: 30 seconds)
    #[serde(with = "duration_secs")]
    pub acquire_timeout: Duration,

    /// Whether to enable connection health checking (default: true)
    pub enable_health_check: bool,
}

fn default_acquire_timeout_secs() -> Duration {
    Duration::from_secs(30)
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_in_flight: None,
            governor_acquire_timeout: default_acquire_timeout_secs(),
            max_connections_per_provider: 10,
            min_idle_connections: 2,
            idle_timeout: Duration::from_secs(300), // 5 minutes
            max_lifetime: Duration::from_secs(1800), // 30 minutes
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

    /// Enable a per-provider in-flight cap of `max` concurrent dispatches.
    ///
    /// Admission happens at the wrap funnel ([`ProviderPool::acquire`] and
    /// [`ProviderPool::try_acquire`]) so every checkout path — pooled or
    /// freshly created clients alike — is governed identically. The permit
    /// spans the whole call, streams included.
    pub fn with_max_in_flight(mut self, max: usize) -> Self {
        self.max_in_flight = Some(max);
        self
    }

    /// Set the wait budget for acquiring an in-flight slot.
    pub fn with_governor_acquire_timeout(mut self, timeout: Duration) -> Self {
        self.governor_acquire_timeout = timeout;
        self
    }

    /// Effective governor configuration for this pool.
    pub fn governor_config(&self) -> GovernorConfig {
        GovernorConfig {
            max_in_flight: self.max_in_flight,
            acquire_timeout: self.governor_acquire_timeout,
        }
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

impl std::fmt::Display for PoolConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PoolConfig(max={}, min_idle={}, health_check={})",
            self.max_connections_per_provider, self.min_idle_connections, self.enable_health_check
        )
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

#[derive(Debug)]
enum BorrowFromIdle {
    Found(PooledClient),
    Exhausted,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ReturnDisposition {
    Returned,
    Dropped,
}

fn borrow_client(idle: &mut Vec<PooledClient>, config: &PoolConfig) -> BorrowFromIdle {
    let mut found_idx = None;
    for (idx, pooled) in idle.iter().enumerate() {
        if health_check(&pooled.meta, config) {
            found_idx = Some(idx);
            break;
        }
    }
    if let Some(idx) = found_idx {
        return BorrowFromIdle::Found(idle.swap_remove(idx));
    }
    idle.retain(|c| health_check(&c.meta, config));
    BorrowFromIdle::Exhausted
}

fn return_client(
    idle: &mut Vec<PooledClient>,
    client: Box<dyn LLMClient>,
    config: &PoolConfig,
) -> ReturnDisposition {
    if idle.len() < config.max_connections_per_provider {
        idle.push(PooledClient {
            client,
            meta: PooledClientMeta::new(),
        });
        ReturnDisposition::Returned
    } else {
        ReturnDisposition::Dropped
    }
}

fn health_check(meta: &PooledClientMeta, config: &PoolConfig) -> bool {
    if !config.enable_health_check {
        return true;
    }
    !meta.is_stale(config)
}

fn pool_stats(
    available: usize,
    in_use: usize,
    total_created: u64,
    max_size: usize,
    borrow_count: u64,
    error_count: u64,
) -> ProviderPoolStats {
    ProviderPoolStats {
        available,
        in_use,
        total: available.saturating_add(in_use),
        total_created,
        max_size,
        borrow_count,
        error_count,
    }
}

fn validate_pooled_client(client: &dyn LLMClient) -> std::result::Result<(), PoolError> {
    if client.model_name().trim().is_empty() {
        return Err(PoolError::InvalidClient {
            reason: "empty model name".to_string(),
        });
    }
    Ok(())
}

/// A pooled LLM client with its metadata
struct PooledClient {
    client: Box<dyn LLMClient>,
    meta: PooledClientMeta,
}

impl std::fmt::Debug for PooledClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledClient")
            .field("meta", &self.meta)
            .finish()
    }
}

/// Pool of clients for a single provider
#[derive(Debug)]
struct ProviderPool {
    /// The provider configuration for creating new clients
    provider: Provider,
    /// Per-provider in-flight admission control (`max_in_flight`).
    governor: Arc<ProviderGovernor>,
    /// Pool of available clients
    clients: Mutex<Vec<PooledClient>>,
    /// Semaphore to limit concurrent connections
    semaphore: Arc<Semaphore>,
    /// Number of clients currently in use
    in_use_count: AtomicUsize,
    /// Total number of clients created (for stats)
    total_created: AtomicU64,
    borrow_count: AtomicU64,
    error_count: AtomicU64,
    /// Configuration reference
    config: PoolConfig,
}

impl ProviderPool {
    fn new(provider: Provider, config: PoolConfig) -> Self {
        let semaphore = Arc::new(Semaphore::new(config.max_connections_per_provider));
        Self {
            provider,
            governor: Arc::new(ProviderGovernor::new(config.governor_config())),
            clients: Mutex::new(Vec::with_capacity(config.max_connections_per_provider)),
            semaphore,
            in_use_count: AtomicUsize::new(0),
            total_created: AtomicU64::new(0),
            borrow_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
            config,
        }
    }

    /// Get an available client from the pool, or create a new one
    async fn acquire(
        &self,
    ) -> std::result::Result<(Box<dyn LLMClient>, OwnedSemaphorePermit), PoolError> {
        let permit = match tokio::time::timeout(
            self.config.acquire_timeout,
            self.semaphore.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                self.error_count.fetch_add(1, Ordering::Relaxed);
                return Err(PoolError::PoolExhausted {
                    max: self.config.max_connections_per_provider,
                });
            }
            Err(_) => {
                self.error_count.fetch_add(1, Ordering::Relaxed);
                return Err(PoolError::Timeout {
                    timeout_ms: self.config.acquire_timeout.as_millis() as u64,
                });
            }
        };

        self.borrow_count.fetch_add(1, Ordering::Relaxed);

        let client = {
            let borrowed = {
                let mut clients = self.clients.lock();
                match borrow_client(&mut clients, &self.config) {
                    BorrowFromIdle::Found(mut pooled) => {
                        validate_pooled_client(pooled.client.as_ref())?;
                        pooled.meta.mark_used();
                        Ok(pooled.client)
                    }
                    BorrowFromIdle::Exhausted => Err(()),
                }
            };
            match borrowed {
                Ok(client) => client,
                Err(()) => {
                    self.total_created.fetch_add(1, Ordering::Relaxed);
                    let created = self.provider.create_client().await.map_err(|e| {
                        self.error_count.fetch_add(1, Ordering::Relaxed);
                        PoolError::InvalidClient {
                            reason: e.to_string(),
                        }
                    })?;
                    validate_pooled_client(created.as_ref())?;
                    created
                }
            }
        };

        self.in_use_count.fetch_add(1, Ordering::Relaxed);
        // Wrap AFTER checkout accounting but BEFORE handing the client out:
        // every consumer of this pool now sees a governed client whose
        // per-dispatch permits are enforced at call time. Unlimited pools
        // get the original client back untouched.
        let client = self.governor.wrap_if_limited(client);
        Ok((client, permit))
    }

    async fn try_acquire(
        &self,
    ) -> std::result::Result<(Box<dyn LLMClient>, OwnedSemaphorePermit), PoolError> {
        let permit = self.semaphore.clone().try_acquire_owned().map_err(|_| {
            self.error_count.fetch_add(1, Ordering::Relaxed);
            PoolError::PoolExhausted {
                max: self.config.max_connections_per_provider,
            }
        })?;

        self.borrow_count.fetch_add(1, Ordering::Relaxed);

        let client = {
            let borrowed = {
                let mut clients = self.clients.lock();
                match borrow_client(&mut clients, &self.config) {
                    BorrowFromIdle::Found(mut pooled) => {
                        validate_pooled_client(pooled.client.as_ref())?;
                        pooled.meta.mark_used();
                        Ok(pooled.client)
                    }
                    BorrowFromIdle::Exhausted => Err(()),
                }
            };
            match borrowed {
                Ok(client) => client,
                Err(()) => {
                    self.total_created.fetch_add(1, Ordering::Relaxed);
                    let created = self.provider.create_client().await.map_err(|e| {
                        self.error_count.fetch_add(1, Ordering::Relaxed);
                        PoolError::InvalidClient {
                            reason: e.to_string(),
                        }
                    })?;
                    validate_pooled_client(created.as_ref())?;
                    created
                }
            }
        };

        self.in_use_count.fetch_add(1, Ordering::Relaxed);
        // Same funnel guarantee as `acquire`: the handed-out client is
        // governed whenever a cap is configured.
        let client = self.governor.wrap_if_limited(client);
        Ok((client, permit))
    }

    /// Return a client to the pool
    fn release(&self, client: Box<dyn LLMClient>) {
        self.in_use_count.fetch_sub(1, Ordering::Relaxed);
        let mut clients = self.clients.lock();
        let _ = return_client(&mut clients, client, &self.config);
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
        pool_stats(
            clients.len(),
            self.in_use_count.load(Ordering::Relaxed),
            self.total_created.load(Ordering::Relaxed),
            self.config.max_connections_per_provider,
            self.borrow_count.load(Ordering::Relaxed),
            self.error_count.load(Ordering::Relaxed),
        )
    }

    /// Drain all connections (for shutdown)
    fn drain(&self) {
        let mut clients = self.clients.lock();
        clients.clear();
    }
}

/// Statistics for a provider pool
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderPoolStats {
    pub available: usize,
    pub in_use: usize,
    pub total: usize,
    pub total_created: u64,
    pub max_size: usize,
    pub borrow_count: u64,
    pub error_count: u64,
}

impl std::fmt::Display for ProviderPoolStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "idle={} active={} total={} created={} max={} borrows={} errors={}",
            self.available,
            self.in_use,
            self.total,
            self.total_created,
            self.max_size,
            self.borrow_count,
            self.error_count
        )
    }
}

/// Overall pool statistics
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PoolStats {
    pub providers: HashMap<String, ProviderPoolStats>,
    pub total_available: usize,
    pub total_in_use: usize,
    pub total_connections: usize,
    pub borrow_count: u64,
    pub error_count: u64,
}

impl std::fmt::Display for PoolStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "providers={} idle={} active={} total={} borrows={} errors={}",
            self.providers.len(),
            self.total_available,
            self.total_in_use,
            self.total_connections,
            self.borrow_count,
            self.error_count
        )
    }
}

/// Guard that returns a client to the pool when dropped
pub struct PooledClientGuard {
    client: Option<Box<dyn LLMClient>>,
    pool: Arc<ProviderPool>,
    _permit: OwnedSemaphorePermit,
}

impl std::fmt::Debug for PooledClientGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledClientGuard")
            .field("has_client", &self.client.is_some())
            .field("pool", &self.pool)
            .finish()
    }
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LLMPoolSnapshot {
    pub config: PoolConfig,
    pub stats: PoolStats,
    pub providers: Vec<String>,
    pub shutdown: bool,
}

pub type LLMPool = ClientPool;

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
        self.get_with_error(provider_name).await.map_err(Into::into)
    }

    pub async fn get_with_error(
        &self,
        provider_name: &str,
    ) -> std::result::Result<PooledClientGuard, PoolError> {
        if self.shutdown.load(Ordering::Relaxed) {
            return Err(PoolError::InvalidClient {
                reason: "pool is shutting down".to_string(),
            });
        }

        let pool = {
            let providers = self.providers.read();
            providers
                .get(provider_name)
                .cloned()
                .ok_or_else(|| PoolError::InvalidClient {
                    reason: format!("provider '{provider_name}' not registered in pool"),
                })?
        };

        let (client, permit) = pool.acquire().await?;

        Ok(PooledClientGuard {
            client: Some(client),
            pool,
            _permit: permit,
        })
    }

    pub async fn try_get(
        &self,
        provider_name: &str,
    ) -> std::result::Result<PooledClientGuard, PoolError> {
        if self.shutdown.load(Ordering::Relaxed) {
            return Err(PoolError::InvalidClient {
                reason: "pool is shutting down".to_string(),
            });
        }

        let pool = {
            let providers = self.providers.read();
            providers
                .get(provider_name)
                .cloned()
                .ok_or_else(|| PoolError::InvalidClient {
                    reason: format!("provider '{provider_name}' not registered in pool"),
                })?
        };

        let (client, permit) = pool.try_acquire().await?;

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
            total_connections: 0,
            borrow_count: 0,
            error_count: 0,
        };

        for (name, pool) in providers.iter() {
            let provider_stats = pool.stats();
            stats.total_available += provider_stats.available;
            stats.total_in_use += provider_stats.in_use;
            stats.total_connections += provider_stats.total;
            stats.borrow_count += provider_stats.borrow_count;
            stats.error_count += provider_stats.error_count;
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

    pub fn snapshot(&self) -> LLMPoolSnapshot {
        LLMPoolSnapshot {
            config: self.config.clone(),
            stats: self.stats(),
            providers: self.provider_names(),
            shutdown: self.is_shutdown(),
        }
    }
}

impl std::fmt::Display for ClientPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let snap = self.snapshot();
        write!(f, "LLMPool(shutdown={}, {})", snap.shutdown, snap.stats)
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
    use crate::client::test_support::MockLLMClient;
    use std::sync::atomic::Ordering as AtomicOrdering;
    fn mock_client(model: &str) -> Box<dyn LLMClient> {
        Box::new(MockLLMClient::new(model))
    }

    fn test_stub_provider() -> Provider {
        Provider::TestStub {
            model: "mock".to_string(),
        }
    }

    fn provider_pool(config: PoolConfig) -> Arc<ProviderPool> {
        Arc::new(ProviderPool::new(test_stub_provider(), config))
    }

    fn seed_pool(pool: &ProviderPool, clients: Vec<Box<dyn LLMClient>>) {
        let mut guard = pool.clients.lock();
        for client in clients {
            guard.push(PooledClient {
                client,
                meta: PooledClientMeta::new(),
            });
        }
    }

    fn register_seeded_pool(
        client_pool: &ClientPool,
        name: &str,
        config: PoolConfig,
        clients: Vec<Box<dyn LLMClient>>,
    ) {
        let sub = provider_pool(config);
        seed_pool(&sub, clients);
        client_pool.providers.write().insert(name.to_string(), sub);
    }

    #[test]
    fn test_pool_config_defaults() {
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
    fn test_pool_config_with_max_lifetime() {
        let config = PoolConfig::default().with_max_lifetime(Duration::from_secs(42));
        assert_eq!(config.max_lifetime, Duration::from_secs(42));
    }

    #[test]
    fn test_pool_config_clone_preserves_fields() {
        let config = PoolConfig::default()
            .with_max_connections(7)
            .with_idle_timeout(Duration::from_secs(11))
            .with_max_lifetime(Duration::from_secs(22))
            .without_health_check();
        let cloned = config.clone();
        assert_eq!(cloned.max_connections_per_provider, 7);
        assert_eq!(cloned.idle_timeout, Duration::from_secs(11));
        assert_eq!(cloned.max_lifetime, Duration::from_secs(22));
        assert!(!cloned.enable_health_check);
    }

    #[test]
    fn test_pool_config_debug_format() {
        let debug = format!("{:?}", PoolConfig::default().with_max_connections(3));
        assert!(debug.contains("max_connections_per_provider"));
        assert!(debug.contains('3'));
    }

    #[test]
    fn test_pool_config_max_lifetime_stale() {
        let config = PoolConfig::default()
            .with_idle_timeout(Duration::from_millis(1))
            .with_max_lifetime(Duration::from_millis(5));
        let meta = PooledClientMeta::new();
        std::thread::sleep(Duration::from_millis(6));
        assert!(meta.is_stale(&config));
    }

    #[test]
    fn test_pooled_client_meta_stale_detection() {
        let config = PoolConfig::default()
            .with_idle_timeout(Duration::from_millis(10))
            .with_max_lifetime(Duration::from_millis(50));

        let meta = PooledClientMeta::new();
        assert!(!meta.is_stale(&config));

        std::thread::sleep(Duration::from_millis(15));
        assert!(meta.is_stale(&config));
    }

    #[test]
    fn test_pooled_client_meta_mark_used_increments() {
        let mut meta = PooledClientMeta::new();
        meta.mark_used();
        meta.mark_used();
        assert_eq!(meta.use_count.load(AtomicOrdering::Relaxed), 2);
    }

    #[test]
    fn test_pooled_client_meta_mark_used_refreshes_idle_timer() {
        let config = PoolConfig::default().with_idle_timeout(Duration::from_millis(30));
        let mut meta = PooledClientMeta::new();
        std::thread::sleep(Duration::from_millis(20));
        meta.mark_used();
        assert!(!meta.is_stale(&config));
    }

    #[test]
    fn test_pooled_client_debug_output() {
        let pooled = PooledClient {
            client: mock_client("debug-model"),
            meta: PooledClientMeta::new(),
        };
        let debug = format!("{pooled:?}");
        assert!(debug.contains("PooledClient"));
        assert!(debug.contains("meta"));
    }

    #[test]
    fn test_provider_pool_stats_clone_and_debug() {
        let stats = pool_stats(2, 1, 5, 10, 3, 1);
        let cloned = stats.clone();
        assert_eq!(cloned.available, 2);
        assert_eq!(cloned.in_use, 1);
        let debug = format!("{stats:?}");
        assert!(debug.contains("available"));
    }

    #[test]
    fn test_pool_stats_clone_and_debug() {
        let mut providers = HashMap::new();
        providers.insert("p1".to_string(), pool_stats(1, 0, 1, 3, 0, 0));
        let stats = PoolStats {
            providers,
            total_available: 1,
            total_in_use: 0,
            total_connections: 1,
            borrow_count: 0,
            error_count: 0,
        };
        let cloned = stats.clone();
        assert_eq!(cloned.total_available, 1);
        assert_eq!(cloned.providers.len(), 1);
        assert!(format!("{stats:?}").contains("total_in_use"));
    }

    #[test]
    fn test_client_pool_default_impl() {
        let pool = ClientPool::default();
        assert!(!pool.is_shutdown());
        assert_eq!(pool.stats().total_available, 0);
    }

    #[test]
    fn test_client_pool_builder_default_impl() {
        let builder = ClientPoolBuilder::default();
        let pool = builder.build();
        assert!(!pool.has_provider("anything"));
    }

    #[test]
    fn test_builder_empty_build() {
        let pool = ClientPoolBuilder::new().build();
        assert!(pool.provider_names().is_empty());
        assert!(!pool.has_provider("missing"));
    }

    #[test]
    fn test_builder_build_arc() {
        let pool = ClientPoolBuilder::new()
            .config(PoolConfig::default().with_max_connections(4))
            .build_arc();
        assert_eq!(pool.stats().total_available, 0);
        assert!(!pool.is_shutdown());
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
    fn test_cleanup_stale_on_empty_pool() {
        let pool = ClientPool::with_defaults();
        assert_eq!(pool.cleanup_stale(), 0);
    }

    #[test]
    fn test_pool_shutdown() {
        let pool = ClientPool::with_defaults();
        assert!(!pool.is_shutdown());
        pool.shutdown();
        assert!(pool.is_shutdown());
    }

    #[test]
    fn test_pool_double_shutdown_is_safe() {
        let pool = ClientPool::with_defaults();
        pool.shutdown();
        pool.shutdown();
        assert!(pool.is_shutdown());
    }

    #[test]
    fn test_provider_registration() {
        let pool = ClientPool::with_defaults();
        pool.register_provider("ollama", test_stub_provider());
        assert!(pool.has_provider("ollama"));
        assert!(!pool.has_provider("openai"));
        assert_eq!(pool.provider_names(), vec!["ollama"]);
    }

    #[test]
    fn test_builder_pattern() {
        let pool = ClientPoolBuilder::new()
            .config(PoolConfig::default().with_max_connections(5))
            .provider("ollama", test_stub_provider())
            .build();
        assert!(pool.has_provider("ollama"));
    }

    #[test]
    fn test_builder_multiple_providers() {
        let pool = ClientPoolBuilder::new()
            .provider("a", test_stub_provider())
            .provider("b", test_stub_provider())
            .build();
        let mut names = pool.provider_names();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn test_get_unregistered_provider_error() {
        let pool = ClientPool::with_defaults();
        let result = pool.get("nonexistent").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::LLM(_)));
    }

    #[tokio::test]
    async fn test_acquire_reuses_seeded_mock_without_network() {
        let config = PoolConfig::default()
            .with_max_connections(2)
            .without_health_check();
        let pool = ClientPool::new(config.clone());
        register_seeded_pool(&pool, "mock", config, vec![mock_client("seeded")]);

        let guard = pool
            .get("mock")
            .await
            .expect("seeded client should be available");
        assert_eq!(guard.client().model_name(), "seeded");
        assert_eq!(pool.stats().providers["mock"].total_created, 0);
    }

    #[tokio::test]
    async fn test_acquire_skips_stale_prefers_first_fresh() {
        let config = PoolConfig::default()
            .with_idle_timeout(Duration::from_millis(5))
            .with_max_lifetime(Duration::from_secs(60));
        let sub = provider_pool(config);

        {
            let mut guard = sub.clients.lock();
            guard.push(PooledClient {
                client: mock_client("stale"),
                meta: PooledClientMeta::new(),
            });
            std::thread::sleep(Duration::from_millis(8));
            guard.push(PooledClient {
                client: mock_client("fresh"),
                meta: PooledClientMeta::new(),
            });
        }

        let (client, permit) = sub.acquire().await.expect("acquire fresh client");
        assert_eq!(client.model_name(), "fresh");
        drop(client);
        drop(permit);
    }

    #[tokio::test]
    async fn test_release_and_reacquire_reuses_pooled_client() {
        let config = PoolConfig::default()
            .with_max_connections(1)
            .without_health_check();
        let pool = ClientPool::new(config.clone());
        register_seeded_pool(&pool, "mock", config, vec![mock_client("reusable")]);

        {
            let guard = pool.get("mock").await.expect("first acquire");
            assert_eq!(guard.client().model_name(), "reusable");
        }

        let guard = pool.get("mock").await.expect("second acquire");
        assert_eq!(guard.client().model_name(), "reusable");
        let stats = pool.stats().providers["mock"].clone();
        assert_eq!(stats.total_created, 0);
        assert_eq!(stats.in_use, 1);
    }

    #[tokio::test]
    async fn test_release_drops_client_when_idle_pool_full() {
        let config = PoolConfig::default()
            .with_max_connections(1)
            .without_health_check();
        let sub = provider_pool(config);
        seed_pool(&sub, vec![mock_client("idle-0")]);

        let (client, permit) = sub.acquire().await.expect("acquire seeded client");
        sub.release(client);
        drop(permit);
        assert_eq!(sub.stats().available, 1);
        assert_eq!(sub.stats().in_use, 0);

        let (overflow, permit2) = sub.acquire().await.expect("acquire again");
        sub.release(overflow);
        drop(permit2);

        let stats = sub.stats();
        assert_eq!(stats.available, 1);
        assert_eq!(stats.in_use, 0);
        assert_eq!(stats.total_created, 0);
    }

    #[tokio::test]
    async fn test_provider_pool_in_use_accounting() {
        let config = PoolConfig::default()
            .with_max_connections(2)
            .without_health_check();
        let sub = provider_pool(config);
        seed_pool(&sub, vec![mock_client("a")]);

        let (_client, permit) = sub.acquire().await.expect("acquire");
        let stats = sub.stats();
        assert_eq!(stats.in_use, 1);
        assert_eq!(stats.available, 0);
        drop(permit);
    }

    #[tokio::test]
    async fn test_cleanup_stale_removes_idle_clients() {
        let config = PoolConfig::default()
            .with_idle_timeout(Duration::from_millis(5))
            .without_health_check();
        let sub = provider_pool(config);
        {
            let mut guard = sub.clients.lock();
            guard.push(PooledClient {
                client: mock_client("old"),
                meta: PooledClientMeta::new(),
            });
            std::thread::sleep(Duration::from_millis(8));
        }
        let removed = sub.cleanup_stale();
        assert_eq!(removed, 1);
        assert_eq!(sub.stats().available, 0);
    }

    #[tokio::test]
    async fn test_pooled_client_guard_debug_and_deref() {
        let config = PoolConfig::default()
            .with_max_connections(1)
            .without_health_check();
        let pool = ClientPool::new(config.clone());
        register_seeded_pool(&pool, "mock", config, vec![mock_client("guard")]);

        let guard = pool.get("mock").await.expect("guard acquire");
        let debug = format!("{guard:?}");
        assert!(debug.contains("PooledClientGuard"));
        assert!(debug.contains("has_client"));
        assert_eq!(guard.model_name(), "guard");
    }

    #[test]
    fn test_register_provider_overwrites_existing_name() {
        let pool = ClientPool::with_defaults();
        pool.register_provider("ollama", test_stub_provider());
        pool.register_provider("ollama", test_stub_provider());
        assert_eq!(pool.provider_names(), vec!["ollama"]);
    }

    #[tokio::test]
    async fn test_stats_aggregate_multiple_providers() {
        let config = PoolConfig::default()
            .with_max_connections(2)
            .without_health_check();
        let pool = ClientPool::new(config.clone());
        register_seeded_pool(&pool, "a", config.clone(), vec![mock_client("a1")]);
        register_seeded_pool(&pool, "b", config, vec![mock_client("b1")]);

        let _ga = pool.get("a").await.expect("provider a");
        let stats = pool.stats();
        assert_eq!(stats.providers.len(), 2);
        assert_eq!(stats.total_in_use, 1);
        assert_eq!(stats.total_available, 1);
    }

    #[tokio::test]
    async fn test_shutdown_drains_seeded_clients() {
        let config = PoolConfig::default()
            .with_max_connections(2)
            .without_health_check();
        let pool = ClientPool::new(config.clone());
        register_seeded_pool(&pool, "mock", config, vec![mock_client("seeded")]);
        assert_eq!(pool.stats().total_available, 1);

        pool.shutdown();
        assert!(pool.is_shutdown());
        assert_eq!(pool.stats().total_available, 0);
    }

    #[tokio::test]
    async fn test_acquire_creates_client_when_pool_empty() {
        let config = PoolConfig::default()
            .with_max_connections(1)
            .without_health_check();
        let sub = provider_pool(config);

        let (client, permit) = sub.acquire().await.expect("create via TestStub");
        assert_eq!(client.model_name(), "mock");
        drop(client);
        drop(permit);
        assert_eq!(sub.stats().total_created, 1);
    }

    #[tokio::test]
    async fn test_pooled_client_guard_client_mut_and_take() {
        let config = PoolConfig::default()
            .with_max_connections(1)
            .without_health_check();
        let pool = ClientPool::new(config.clone());
        register_seeded_pool(&pool, "mock", config, vec![mock_client("guard-mut")]);

        let mut guard = pool.get("mock").await.expect("guard acquire");
        assert_eq!(guard.client_mut().model_name(), "guard-mut");
        let taken = guard.take();
        assert_eq!(taken.model_name(), "guard-mut");
    }

    fn serde_roundtrip<T>(value: &T) -> T
    where
        T: serde::Serialize + for<'de> serde::Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).unwrap();
        let parsed: T = serde_json::from_str(&json).unwrap();
        assert_eq!(*value, parsed);
        parsed
    }

    #[test]
    fn test_pool_config_serde_roundtrip() {
        let config = PoolConfig::default()
            .with_max_connections(4)
            .with_idle_timeout(Duration::from_secs(90))
            .without_health_check();
        serde_roundtrip(&config);
    }

    #[test]
    fn test_pool_stats_serde_roundtrip() {
        let mut providers = HashMap::new();
        providers.insert("mock".into(), pool_stats(1, 2, 3, 4, 5, 6));
        let stats = PoolStats {
            providers,
            total_available: 1,
            total_in_use: 2,
            total_connections: 3,
            borrow_count: 5,
            error_count: 6,
        };
        serde_roundtrip(&stats);
    }

    #[test]
    fn test_provider_pool_stats_serde_roundtrip() {
        serde_roundtrip(&pool_stats(2, 1, 9, 10, 7, 2));
    }

    #[test]
    fn test_llm_pool_snapshot_serde_roundtrip() {
        let pool = ClientPoolBuilder::new()
            .provider("mock", test_stub_provider())
            .build();
        serde_roundtrip(&pool.snapshot());
    }

    #[test]
    fn test_llm_pool_type_alias() {
        let pool: LLMPool = ClientPool::with_defaults();
        assert_eq!(pool.stats().total_available, 0);
    }

    #[test]
    fn test_pool_error_serde_roundtrip_exhausted() {
        serde_roundtrip(&PoolError::PoolExhausted { max: 3 });
    }

    #[test]
    fn test_pool_error_serde_roundtrip_timeout() {
        serde_roundtrip(&PoolError::Timeout { timeout_ms: 250 });
    }

    #[test]
    fn test_pool_error_serde_roundtrip_invalid_client() {
        serde_roundtrip(&PoolError::InvalidClient {
            reason: "bad".into(),
        });
    }

    #[test]
    fn test_pool_error_display_variants() {
        assert!(
            PoolError::PoolExhausted { max: 2 }
                .to_string()
                .contains("pool exhausted")
        );
        assert!(
            PoolError::Timeout { timeout_ms: 10 }
                .to_string()
                .contains("timeout")
        );
        assert!(
            PoolError::InvalidClient { reason: "x".into() }
                .to_string()
                .contains("invalid")
        );
    }

    #[test]
    fn test_pool_error_clone_debug() {
        let err = PoolError::PoolExhausted { max: 1 };
        assert_eq!(err, err.clone());
        assert!(format!("{err:?}").contains("PoolExhausted"));
    }

    #[test]
    fn test_pool_config_display() {
        let s = PoolConfig::default().with_max_connections(6).to_string();
        assert!(s.contains("max=6"));
    }

    #[test]
    fn test_pool_stats_display() {
        let stats = PoolStats {
            providers: HashMap::new(),
            total_available: 1,
            total_in_use: 2,
            total_connections: 3,
            borrow_count: 4,
            error_count: 5,
        };
        let s = stats.to_string();
        assert!(s.contains("idle=1"));
        assert!(s.contains("errors=5"));
    }

    #[test]
    fn test_provider_pool_stats_display() {
        let s = pool_stats(1, 2, 0, 4, 9, 1).to_string();
        assert!(s.contains("borrows=9"));
        assert!(s.contains("total=3"));
    }

    #[test]
    fn test_client_pool_display() {
        let pool = ClientPool::with_defaults();
        let s = pool.to_string();
        assert!(s.contains("LLMPool"));
        assert!(s.contains("shutdown=false"));
    }

    #[test]
    fn test_borrow_client_returns_first_healthy() {
        let config = PoolConfig::default().without_health_check();
        let mut idle = vec![PooledClient {
            client: mock_client("a"),
            meta: PooledClientMeta::new(),
        }];
        match borrow_client(&mut idle, &config) {
            BorrowFromIdle::Found(p) => assert_eq!(p.client.model_name(), "a"),
            _ => panic!("expected found"),
        }
        assert!(idle.is_empty());
    }

    #[test]
    fn test_borrow_client_purges_stale_entries() {
        let config = PoolConfig::default().with_idle_timeout(Duration::from_millis(1));
        let mut idle = vec![PooledClient {
            client: mock_client("stale"),
            meta: PooledClientMeta::new(),
        }];
        std::thread::sleep(Duration::from_millis(3));
        assert!(matches!(
            borrow_client(&mut idle, &config),
            BorrowFromIdle::Exhausted
        ));
        assert!(idle.is_empty());
    }

    #[test]
    fn test_return_client_respects_capacity() {
        let config = PoolConfig::default()
            .with_max_connections(1)
            .without_health_check();
        let mut idle = vec![];
        assert_eq!(
            return_client(&mut idle, mock_client("a"), &config),
            ReturnDisposition::Returned
        );
        assert_eq!(
            return_client(&mut idle, mock_client("b"), &config),
            ReturnDisposition::Dropped
        );
        assert_eq!(idle.len(), 1);
    }

    #[test]
    fn test_health_check_disabled_ignores_stale_meta() {
        let config = PoolConfig::default()
            .with_idle_timeout(Duration::from_millis(1))
            .without_health_check();
        let meta = PooledClientMeta::new();
        std::thread::sleep(Duration::from_millis(3));
        assert!(health_check(&meta, &config));
    }

    #[test]
    fn test_health_check_enabled_rejects_stale_meta() {
        let config = PoolConfig::default().with_idle_timeout(Duration::from_millis(1));
        let meta = PooledClientMeta::new();
        std::thread::sleep(Duration::from_millis(3));
        assert!(!health_check(&meta, &config));
    }

    #[test]
    fn test_pool_stats_helper_totals() {
        let stats = pool_stats(2, 3, 10, 8, 4, 1);
        assert_eq!(stats.total, 5);
        assert_eq!(stats.borrow_count, 4);
        assert_eq!(stats.error_count, 1);
    }

    #[test]
    fn test_validate_pooled_client_rejects_empty_model() {
        let client = mock_client("");
        let err = validate_pooled_client(client.as_ref()).unwrap_err();
        assert!(matches!(err, PoolError::InvalidClient { .. }));
    }

    #[test]
    fn test_validate_pooled_client_accepts_named_model() {
        validate_pooled_client(mock_client("ok").as_ref()).unwrap();
    }

    #[tokio::test]
    async fn test_try_get_pool_exhausted_when_at_capacity() {
        let config = PoolConfig::default()
            .with_max_connections(1)
            .without_health_check();
        let pool = ClientPool::new(config.clone());
        register_seeded_pool(&pool, "mock", config, vec![mock_client("only")]);

        let _guard = pool.try_get("mock").await.expect("first borrow");
        let err = pool.try_get("mock").await.unwrap_err();
        assert!(matches!(err, PoolError::PoolExhausted { max: 1 }));
        assert_eq!(pool.stats().providers["mock"].error_count, 1);
    }

    #[tokio::test]
    async fn test_acquire_timeout_increments_error_count() {
        let mut config = PoolConfig::default()
            .with_max_connections(1)
            .with_idle_timeout(Duration::from_secs(60))
            .without_health_check();
        config.acquire_timeout = Duration::from_millis(50);
        let sub = provider_pool(config);
        seed_pool(&sub, vec![mock_client("held")]);

        let (_c, permit) = sub.acquire().await.expect("hold permit");
        let err = match sub.acquire().await {
            Err(e) => e,
            Ok(_) => panic!("expected pool acquire timeout"),
        };
        assert!(matches!(err, PoolError::Timeout { .. }));
        assert_eq!(sub.stats().error_count, 1);
        drop(permit);
    }

    #[tokio::test]
    async fn test_borrow_count_increments_on_success() {
        let config = PoolConfig::default()
            .with_max_connections(2)
            .without_health_check();
        let sub = provider_pool(config);
        seed_pool(&sub, vec![mock_client("x")]);
        let (_c, permit) = sub.acquire().await.unwrap();
        drop(permit);
        assert_eq!(sub.stats().borrow_count, 1);
    }

    #[tokio::test]
    async fn test_concurrent_borrows_respect_max_connections() {
        let config = PoolConfig::default()
            .with_max_connections(2)
            .without_health_check();
        let pool = Arc::new(ClientPool::new(config.clone()));
        register_seeded_pool(
            &pool,
            "mock",
            config,
            vec![mock_client("c1"), mock_client("c2")],
        );

        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let pool_bg = Arc::clone(&pool);
        tokio::spawn(async move {
            let result = pool_bg.get("mock").await;
            let _ = tx.send(result);
        });

        let g1 = pool.get("mock").await.expect("first");
        let g2 = pool.get("mock").await.expect("second");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut rx)
                .await
                .is_err(),
            "third borrow should still be waiting"
        );
        drop(g1);
        drop(g2);
        let third_result = rx
            .await
            .expect("channel")
            .expect("third completes after release");
        drop(third_result);
    }

    #[tokio::test]
    async fn test_get_with_error_unregistered_invalid_client() {
        let pool = ClientPool::with_defaults();
        let err = pool.get_with_error("missing").await.unwrap_err();
        assert!(matches!(err, PoolError::InvalidClient { .. }));
    }

    #[tokio::test]
    async fn test_get_with_error_shutdown_invalid_client() {
        let pool = ClientPool::with_defaults();
        pool.shutdown();
        let err = pool.get_with_error("anything").await.unwrap_err();
        assert!(matches!(err, PoolError::InvalidClient { .. }));
    }

    #[tokio::test]
    async fn test_cleanup_stale_client_pool_aggregates() {
        let config = PoolConfig::default()
            .with_idle_timeout(Duration::from_millis(5))
            .without_health_check();
        let pool = ClientPool::new(config.clone());
        register_seeded_pool(&pool, "mock", config, vec![mock_client("old")]);
        std::thread::sleep(Duration::from_millis(8));
        assert_eq!(pool.cleanup_stale(), 1);
        assert_eq!(pool.stats().total_available, 0);
    }

    #[tokio::test]
    async fn test_stats_track_aggregate_borrow_and_error_counts() {
        let config = PoolConfig::default()
            .with_max_connections(1)
            .without_health_check();
        let pool = ClientPool::new(config.clone());
        register_seeded_pool(&pool, "mock", config, vec![mock_client("one")]);
        let _g = pool.try_get("mock").await.unwrap();
        let _ = pool.try_get("mock").await;
        let stats = pool.stats();
        assert_eq!(stats.borrow_count, 1);
        assert_eq!(stats.error_count, 1);
    }

    #[tokio::test]
    async fn test_acquire_removes_stale_before_creating_client() {
        let config = PoolConfig::default().with_idle_timeout(Duration::from_millis(5));
        let sub = provider_pool(config);
        {
            let mut guard = sub.clients.lock();
            guard.push(PooledClient {
                client: mock_client("stale-only"),
                meta: PooledClientMeta::new(),
            });
            std::thread::sleep(Duration::from_millis(8));
        }
        let (client, permit) = sub.acquire().await.expect("creates fresh client");
        assert_eq!(client.model_name(), "mock");
        assert_eq!(sub.stats().total_created, 1);
        drop(client);
        drop(permit);
    }

    #[tokio::test]
    async fn test_race_release_then_reacquire() {
        let config = PoolConfig::default()
            .with_max_connections(1)
            .without_health_check();
        let pool = Arc::new(ClientPool::new(config.clone()));
        register_seeded_pool(&pool, "mock", config, vec![mock_client("race")]);

        let g1 = pool.get("mock").await.expect("first");
        let pool2 = Arc::clone(&pool);
        let j = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            pool2.get("mock").await
        });
        drop(g1);
        let g2 = j
            .await
            .expect("join")
            .expect("second acquire after release");
        assert_eq!(g2.model_name(), "race");
    }

    #[test]
    fn test_return_disposition_debug_clone() {
        let d = ReturnDisposition::Returned;
        assert_eq!(d, d);
        assert!(format!("{d:?}").contains("Returned"));
    }

    #[test]
    fn test_pool_stats_clone_preserves_aggregate_fields() {
        let stats = PoolStats {
            providers: HashMap::new(),
            total_available: 0,
            total_in_use: 0,
            total_connections: 0,
            borrow_count: 2,
            error_count: 3,
        };
        let cloned = stats.clone();
        assert_eq!(cloned.borrow_count, 2);
        assert_eq!(cloned.error_count, 3);
    }

    #[tokio::test]
    async fn test_provider_pool_stats_after_release_shows_idle() {
        let config = PoolConfig::default()
            .with_max_connections(1)
            .without_health_check();
        let sub = provider_pool(config);
        let (client, permit) = sub.acquire().await.unwrap();
        sub.release(client);
        drop(permit);
        let stats = sub.stats();
        assert_eq!(stats.available, 1);
        assert_eq!(stats.in_use, 0);
        assert_eq!(stats.total, 1);
    }

    #[test]
    fn test_pool_stats_default_aggregate_fields() {
        let stats = ClientPool::with_defaults().stats();
        assert_eq!(stats.total_connections, 0);
        assert_eq!(stats.borrow_count, 0);
        assert_eq!(stats.error_count, 0);
    }

    #[test]
    fn test_return_disposition_variants() {
        assert_ne!(ReturnDisposition::Returned, ReturnDisposition::Dropped);
    }

    #[tokio::test]
    async fn test_try_get_success_returns_guard() {
        let config = PoolConfig::default()
            .with_max_connections(1)
            .without_health_check();
        let pool = ClientPool::new(config.clone());
        register_seeded_pool(&pool, "mock", config, vec![mock_client("ok")]);
        let guard = pool.try_get("mock").await.expect("try_get ok");
        assert_eq!(guard.model_name(), "ok");
    }

    #[tokio::test]
    async fn test_get_with_error_success_path() {
        let config = PoolConfig::default()
            .with_max_connections(1)
            .without_health_check();
        let pool = ClientPool::new(config.clone());
        register_seeded_pool(&pool, "mock", config, vec![mock_client("ok2")]);
        let guard = pool.get_with_error("mock").await.expect("get ok");
        assert_eq!(guard.model_name(), "ok2");
    }

    #[test]
    fn test_llm_pool_snapshot_lists_providers() {
        let pool = ClientPoolBuilder::new()
            .provider("a", test_stub_provider())
            .provider("b", test_stub_provider())
            .build();
        let snap = pool.snapshot();
        let mut names = snap.providers;
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn test_health_check_fresh_client_meta() {
        let config = PoolConfig::default().with_idle_timeout(Duration::from_secs(60));
        let meta = PooledClientMeta::new();
        assert!(health_check(&meta, &config));
    }

    #[tokio::test]
    async fn test_cleanup_stale_on_provider_pool() {
        let config = PoolConfig::default().with_idle_timeout(Duration::from_millis(5));
        let sub = provider_pool(config);
        {
            let mut guard = sub.clients.lock();
            guard.push(PooledClient {
                client: mock_client("gone"),
                meta: PooledClientMeta::new(),
            });
            std::thread::sleep(Duration::from_millis(8));
        }
        assert_eq!(sub.cleanup_stale(), 1);
    }

    #[tokio::test]
    async fn test_get_after_shutdown() {
        let pool = ClientPool::with_defaults();
        pool.shutdown();

        let result = pool.get("anything").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::LLM(_)));
    }

    // ===== Per-provider in-flight governor =====
    use crate::client::LLMResponse;
    use ares_types::types::ToolDefinition;
    use futures::Stream;

    /// Slow mock client: each `generate` sleeps then records the moment it
    /// runs, so a test can observe true concurrency (overlapping dispatches).
    struct SlowMockClient {
        delay: Duration,
        in_flight: Arc<AtomicUsize>,
        max_observed: Arc<AtomicUsize>,
    }

    impl SlowMockClient {
        fn new(
            delay: Duration,
            in_flight: Arc<AtomicUsize>,
            max_observed: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                delay,
                in_flight,
                max_observed,
            }
        }
    }

    #[async_trait]
    impl LLMClient for SlowMockClient {
        async fn generate(&self, _prompt: &str) -> Result<String> {
            let now = self.in_flight.fetch_add(1, AtomicOrdering::Relaxed) + 1;
            self.max_observed.fetch_max(now, AtomicOrdering::Relaxed);
            tokio::time::sleep(self.delay).await;
            self.in_flight.fetch_sub(1, AtomicOrdering::Relaxed);
            Ok("slow".into())
        }

        async fn generate_with_system(&self, _system: &str, prompt: &str) -> Result<String> {
            self.generate(prompt).await
        }

        async fn generate_with_history(
            &self,
            messages: &[(String, String)],
        ) -> Result<LLMResponse> {
            let content = self
                .generate(messages.first().map(|(_, c)| c.as_str()).unwrap_or(""))
                .await?;
            Ok(LLMResponse {
                content,
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }

        async fn generate_with_tools(
            &self,
            prompt: &str,
            _tools: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            let content = self.generate(prompt).await?;
            Ok(LLMResponse {
                content,
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }

        async fn generate_with_tools_and_history(
            &self,
            messages: &[crate::coordinator::ConversationMessage],
            _tools: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            let content = self
                .generate(messages.first().map(|m| m.content.as_str()).unwrap_or(""))
                .await?;
            Ok(LLMResponse {
                content,
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }

        async fn stream(
            &self,
            _prompt: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("stream unused here".into()))
        }

        async fn stream_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("stream unused here".into()))
        }

        async fn stream_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("stream unused here".into()))
        }

        fn model_name(&self) -> &str {
            "slow-mock"
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn governor_caps_concurrent_dispatch() {
        const PERMITS: usize = 2;
        const CALLS: usize = 8;

        // Every task gets its own SLOW client (seeded, so no network-backed
        // creation happens): without the governor all eight 30ms bodies would
        // overlap and the high-water mark would hit 8.
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_observed = Arc::new(AtomicUsize::new(0));
        let slow_clients: Vec<Box<dyn LLMClient>> = (0..CALLS)
            .map(|_| {
                Box::new(SlowMockClient::new(
                    Duration::from_millis(30),
                    Arc::clone(&in_flight),
                    Arc::clone(&max_observed),
                )) as Box<dyn LLMClient>
            })
            .collect();

        let config = PoolConfig::default()
            .with_max_in_flight(PERMITS)
            .with_governor_acquire_timeout(Duration::from_secs(5))
            .without_health_check();
        let pool = Arc::new(ClientPool::new(config.clone()));
        register_seeded_pool(&pool, "mock", config, slow_clients);

        let mut handles = Vec::new();
        for _ in 0..CALLS {
            let pool = Arc::clone(&pool);
            handles.push(tokio::spawn(async move {
                let guard = pool.get("mock").await.expect("governed checkout");
                let _ = guard.client().generate("hello").await;
            }));
        }
        for handle in handles {
            handle.await.expect("task joins");
        }

        let observed = max_observed.load(AtomicOrdering::Relaxed);
        assert_eq!(
            in_flight.load(AtomicOrdering::Relaxed),
            0,
            "all dispatches finished"
        );
        assert!(
            observed <= PERMITS,
            "max observed in-flight ({observed}) exceeded permits ({PERMITS})"
        );
        assert_eq!(observed, PERMITS, "cap should be reached under load");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unlimited_default_preserves_behavior() {
        // Default config: no cap configured. Many overlapping checkouts must
        // all proceed without waiting on any governor slot.
        let config = PoolConfig::default().without_health_check();
        let pool = ClientPool::new(config.clone());
        register_seeded_pool(&pool, "mock", config, vec![mock_client("free")]);

        let guard = pool.get("mock").await.expect("checkout works");
        assert_eq!(guard.client().model_name(), "free");
        // The handed-out client is NOT a governor wrapper — no extra layer.
        let taken: Box<dyn LLMClient> = guard.take();
        assert_eq!(taken.model_name(), "free");
        drop(taken);

        // And the wrap funnel itself is a pass-through when unlimited.
        let unlimited = ProviderGovernor::new(GovernorConfig::default());
        let wrapped = unlimited.wrap_if_limited(mock_client("passthrough"));
        assert_eq!(
            wrapped.model_name(),
            "passthrough",
            "unlimited governors must not install wrappers"
        );

        let defaults = PoolConfig::default();
        assert_eq!(defaults.max_in_flight, None);
        assert_eq!(defaults.governor_config(), GovernorConfig::default());
    }

    #[tokio::test]
    async fn permit_released_on_error_path() {
        let config = PoolConfig::default()
            .with_max_in_flight(1)
            .with_governor_acquire_timeout(Duration::from_millis(100))
            .without_health_check();
        let sub = provider_pool(config.clone());

        // Take the only slot with a failing call: generate errors after the
        // admit. The permit must return even though the dispatch failed.
        let failing = SlowMockFailing;
        let guarded = sub.governor.wrap_if_limited(Box::new(failing));
        let err = guarded.generate("boom").await.unwrap_err();
        assert!(matches!(err, AppError::LLM(_)));

        // The single slot is free again — an immediate second admission
        // succeeds without hitting the 100ms timeout.
        let ok_client = sub.governor.wrap_if_limited(Box::new(SlowMockClient::new(
            Duration::from_millis(1),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        )));
        let started = std::time::Instant::now();
        ok_client.generate("fine").await.expect("slot was released");
        assert!(
            started.elapsed() < Duration::from_millis(90),
            "second dispatch should not wait: slot was released by the error path"
        );
    }

    /// Client that always fails AFTER being admitted.
    struct SlowMockFailing;

    #[async_trait]
    impl LLMClient for SlowMockFailing {
        async fn generate(&self, _prompt: &str) -> Result<String> {
            Err(AppError::LLM("upstream rejected".into()))
        }

        async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
            Err(AppError::LLM("upstream rejected".into()))
        }

        async fn generate_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> Result<LLMResponse> {
            Err(AppError::LLM("upstream rejected".into()))
        }

        async fn generate_with_tools(
            &self,
            _prompt: &str,
            _tools: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            Err(AppError::LLM("upstream rejected".into()))
        }

        async fn generate_with_tools_and_history(
            &self,
            _messages: &[crate::coordinator::ConversationMessage],
            _tools: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            Err(AppError::LLM("upstream rejected".into()))
        }

        async fn stream(
            &self,
            _prompt: &str,
        ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("stream failed at setup".into()))
        }

        async fn stream_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("stream failed at setup".into()))
        }

        async fn stream_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("stream failed at setup".into()))
        }

        fn model_name(&self) -> &str {
            "failing-mock"
        }
    }
}
