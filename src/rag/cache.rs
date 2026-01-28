//! Embedding Cache for RAG Pipeline
//!
//! This module provides caching for text embeddings to avoid re-computing
//! vectors for unchanged content. This is especially valuable for:
//!
//! - Large document re-indexing
//! - Frequently accessed documents
//! - Multi-collection setups with shared documents
//!
//! # Cache Key Strategy
//!
//! Cache keys are computed as SHA-256 hashes of `text + model_name` to ensure:
//! - Unique keys for different content
//! - Model-specific embeddings (different models produce different vectors)
//! - Consistent keys across restarts
//!
//! # Implementation
//!
//! Uses the `lru` crate for O(1) get/put operations with proper LRU eviction.
//! The cache is thread-safe via `parking_lot::Mutex`.
//!
//! # Example
//!
//! ```ignore
//! use ares::rag::cache::{EmbeddingCache, LruEmbeddingCache, CacheConfig};
//!
//! // Create a cache with 512MB max size
//! let cache = LruEmbeddingCache::new(CacheConfig {
//!     max_size_bytes: 512 * 1024 * 1024,
//!     ..Default::default()
//! });
//!
//! // Check cache before computing embedding
//! let key = cache.compute_key("hello world", "bge-small-en-v1.5");
//! if let Some(embedding) = cache.get(&key).await {
//!     // Use cached embedding
//! } else {
//!     // Compute and cache
//!     let embedding = embed("hello world").await?;
//!     cache.set(&key, embedding.clone(), None).await?;
//! }
//! ```

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use lru::LruCache;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::types::Result;

// ============================================================================
// Cache Types
// ============================================================================

/// Statistics for cache performance monitoring
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheStats {
    /// Number of cache hits
    pub hits: u64,
    /// Number of cache misses
    pub misses: u64,
    /// Current size in bytes (approximate)
    pub size_bytes: u64,
    /// Number of entries in cache
    pub entry_count: usize,
    /// Number of evictions due to capacity
    pub evictions: u64,
}

impl CacheStats {
    /// Calculate hit rate as a percentage
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            (self.hits as f64 / total as f64) * 100.0
        }
    }
}

/// Configuration for the embedding cache
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Maximum cache size in bytes (default: 256MB)
    #[serde(default = "default_max_size_bytes")]
    pub max_size_bytes: u64,

    /// Default TTL for cache entries (None = no expiry)
    #[serde(default)]
    pub default_ttl: Option<Duration>,

    /// Whether the cache is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_max_size_bytes() -> u64 {
    256 * 1024 * 1024 // 256 MB
}

fn default_enabled() -> bool {
    true
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_size_bytes: default_max_size_bytes(),
            default_ttl: None,
            enabled: default_enabled(),
        }
    }
}

// ============================================================================
// Cache Trait
// ============================================================================

/// Trait for embedding cache implementations
///
/// This trait defines the interface for caching embeddings. Implementations
/// can use different backends (in-memory, Redis, disk, etc.).
pub trait EmbeddingCache: Send + Sync {
    /// Get an embedding from the cache
    fn get(&self, key: &str) -> Option<Vec<f32>>;

    /// Store an embedding in the cache with optional TTL
    fn set(&self, key: &str, embedding: Vec<f32>, ttl: Option<Duration>) -> Result<()>;

    /// Remove an entry from the cache
    fn invalidate(&self, key: &str) -> Result<()>;

    /// Clear all entries from the cache
    fn clear(&self) -> Result<()>;

    /// Get cache statistics
    fn stats(&self) -> CacheStats;

    /// Compute a cache key for the given text and model
    fn compute_key(&self, text: &str, model: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        hasher.update(b"|");
        hasher.update(model.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Check if the cache is enabled
    fn is_enabled(&self) -> bool;
}

// ============================================================================
// LRU Cache Entry
// ============================================================================

/// A cache entry with metadata for expiration
#[derive(Debug, Clone)]
struct CacheEntry {
    /// The cached embedding vector
    embedding: Vec<f32>,
    /// Optional expiry time
    expires_at: Option<Instant>,
    /// Size in bytes (approximate)
    size_bytes: usize,
}

impl CacheEntry {
    fn new(embedding: Vec<f32>, ttl: Option<Duration>) -> Self {
        let now = Instant::now();
        let size_bytes = embedding.len() * std::mem::size_of::<f32>();
        Self {
            embedding,
            expires_at: ttl.map(|d| now + d),
            size_bytes,
        }
    }

    fn is_expired(&self) -> bool {
        self.expires_at
            .map(|exp| Instant::now() > exp)
            .unwrap_or(false)
    }
}

// ============================================================================
// LRU Embedding Cache
// ============================================================================

/// Default maximum number of entries in the LRU cache
const DEFAULT_MAX_ENTRIES: usize = 10_000;

/// In-memory LRU cache for embeddings
///
/// Uses the `lru` crate for O(1) get/put operations with proper LRU eviction.
/// Thread-safe via `parking_lot::Mutex`.
///
/// # Memory Management
///
/// The cache limits entries by count (not bytes) for simplicity and O(1) operations.
/// The `max_size_bytes` config is used to estimate max entries based on average
/// embedding size (assuming 384-dimensional embeddings = 1536 bytes each).
pub struct LruEmbeddingCache {
    /// The LRU cache storage (key -> CacheEntry)
    cache: Mutex<LruCache<String, CacheEntry>>,
    /// Configuration
    config: CacheConfig,
    /// Current size in bytes (approximate)
    current_size: AtomicU64,
    /// Cache hit counter
    hits: AtomicU64,
    /// Cache miss counter
    misses: AtomicU64,
    /// Eviction counter
    evictions: AtomicU64,
}

impl LruEmbeddingCache {
    /// Create a new LRU embedding cache with the given configuration
    pub fn new(config: CacheConfig) -> Self {
        // Estimate max entries from max_size_bytes
        // Assume average embedding is 384 dimensions = 1536 bytes
        let avg_entry_size = 384 * std::mem::size_of::<f32>(); // 1536 bytes
        let max_entries = (config.max_size_bytes as usize / avg_entry_size).max(100);
        let capacity = NonZeroUsize::new(max_entries)
            .unwrap_or(NonZeroUsize::new(DEFAULT_MAX_ENTRIES).unwrap());

        Self {
            cache: Mutex::new(LruCache::new(capacity)),
            config,
            current_size: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }

    /// Create a cache with default configuration
    pub fn with_defaults() -> Self {
        Self::new(CacheConfig::default())
    }

    /// Create a cache with a specific max size in bytes
    pub fn with_max_size(max_size_bytes: u64) -> Self {
        Self::new(CacheConfig {
            max_size_bytes,
            ..Default::default()
        })
    }

    /// Create a cache with a specific max entry count
    pub fn with_max_entries(max_entries: usize) -> Self {
        let capacity = NonZeroUsize::new(max_entries)
            .unwrap_or(NonZeroUsize::new(DEFAULT_MAX_ENTRIES).unwrap());
        Self {
            cache: Mutex::new(LruCache::new(capacity)),
            config: CacheConfig::default(),
            current_size: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }

    /// Remove expired entries from the cache
    pub fn cleanup_expired(&self) {
        let mut cache = self.cache.lock();
        let mut expired_keys = Vec::new();

        // Collect expired keys (can't remove while iterating)
        for (key, entry) in cache.iter() {
            if entry.is_expired() {
                expired_keys.push(key.clone());
            }
        }

        // Remove expired entries
        for key in expired_keys {
            if let Some(entry) = cache.pop(&key) {
                self.current_size
                    .fetch_sub(entry.size_bytes as u64, Ordering::Relaxed);
            }
        }
    }

    /// Get the current cache size in bytes
    pub fn size_bytes(&self) -> u64 {
        self.current_size.load(Ordering::Relaxed)
    }

    /// Get the number of entries in the cache
    pub fn len(&self) -> usize {
        self.cache.lock().len()
    }

    /// Check if the cache is empty
    pub fn is_empty(&self) -> bool {
        self.cache.lock().is_empty()
    }
}

impl EmbeddingCache for LruEmbeddingCache {
    fn get(&self, key: &str) -> Option<Vec<f32>> {
        if !self.config.enabled {
            return None;
        }

        let mut cache = self.cache.lock();

        // get() in lru crate automatically promotes to most recently used
        if let Some(entry) = cache.get(key) {
            if entry.is_expired() {
                // Remove expired entry
                let entry = cache.pop(key).unwrap();
                self.current_size
                    .fetch_sub(entry.size_bytes as u64, Ordering::Relaxed);
                self.misses.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            self.hits.fetch_add(1, Ordering::Relaxed);
            Some(entry.embedding.clone())
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    fn set(&self, key: &str, embedding: Vec<f32>, ttl: Option<Duration>) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let entry = CacheEntry::new(embedding, ttl.or(self.config.default_ttl));
        let entry_size = entry.size_bytes;

        let mut cache = self.cache.lock();

        // Remove old entry if exists (to update size tracking)
        if let Some(old_entry) = cache.pop(key) {
            self.current_size
                .fetch_sub(old_entry.size_bytes as u64, Ordering::Relaxed);
        }

        // Check if cache is at capacity before push
        let was_at_capacity = cache.len() == cache.cap().get();

        // Push new entry (LRU eviction happens automatically if at capacity)
        if let Some((_, evicted)) = cache.push(key.to_string(), entry) {
            // An entry was evicted
            self.current_size
                .fetch_sub(evicted.size_bytes as u64, Ordering::Relaxed);
            self.evictions.fetch_add(1, Ordering::Relaxed);
        } else if was_at_capacity {
            // We were at capacity but push didn't return evicted (shouldn't happen)
            // but handle it just in case
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }

        // Update size
        self.current_size
            .fetch_add(entry_size as u64, Ordering::Relaxed);

        Ok(())
    }

    fn invalidate(&self, key: &str) -> Result<()> {
        let mut cache = self.cache.lock();
        if let Some(entry) = cache.pop(key) {
            self.current_size
                .fetch_sub(entry.size_bytes as u64, Ordering::Relaxed);
        }
        Ok(())
    }

    fn clear(&self) -> Result<()> {
        let mut cache = self.cache.lock();
        cache.clear();
        self.current_size.store(0, Ordering::Relaxed);
        Ok(())
    }

    fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            size_bytes: self.current_size.load(Ordering::Relaxed),
            entry_count: self.cache.lock().len(),
            evictions: self.evictions.load(Ordering::Relaxed),
        }
    }

    fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

// ============================================================================
// No-Op Cache
// ============================================================================

/// A no-op cache that doesn't store anything
///
/// Useful for disabling caching without changing the code structure.
#[derive(Debug, Default)]
pub struct NoOpCache;

impl NoOpCache {
    /// Create a new no-op cache
    pub fn new() -> Self {
        Self
    }
}

impl EmbeddingCache for NoOpCache {
    fn get(&self, _key: &str) -> Option<Vec<f32>> {
        None
    }

    fn set(&self, _key: &str, _embedding: Vec<f32>, _ttl: Option<Duration>) -> Result<()> {
        Ok(())
    }

    fn invalidate(&self, _key: &str) -> Result<()> {
        Ok(())
    }

    fn clear(&self) -> Result<()> {
        Ok(())
    }

    fn stats(&self) -> CacheStats {
        CacheStats::default()
    }

    fn is_enabled(&self) -> bool {
        false
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_key_computation() {
        let cache = LruEmbeddingCache::with_defaults();

        let key1 = cache.compute_key("hello world", "bge-small-en-v1.5");
        let key2 = cache.compute_key("hello world", "bge-small-en-v1.5");
        let key3 = cache.compute_key("hello world", "bge-base-en-v1.5");
        let key4 = cache.compute_key("different text", "bge-small-en-v1.5");

        // Same input should produce same key
        assert_eq!(key1, key2);
        // Different model should produce different key
        assert_ne!(key1, key3);
        // Different text should produce different key
        assert_ne!(key1, key4);
    }

    #[test]
    fn test_cache_set_and_get() {
        let cache = LruEmbeddingCache::with_defaults();
        let key = "test_key";
        let embedding = vec![1.0, 2.0, 3.0, 4.0];

        // Initially empty
        assert!(cache.get(key).is_none());
        assert_eq!(cache.stats().misses, 1);

        // Set and get
        cache.set(key, embedding.clone(), None).unwrap();
        let retrieved = cache.get(key);

        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), embedding);
        assert_eq!(cache.stats().hits, 1);
    }

    #[test]
    fn test_cache_invalidate() {
        let cache = LruEmbeddingCache::with_defaults();
        let key = "test_key";
        let embedding = vec![1.0, 2.0, 3.0];

        cache.set(key, embedding, None).unwrap();
        assert!(cache.get(key).is_some());

        cache.invalidate(key).unwrap();
        assert!(cache.get(key).is_none());
    }

    #[test]
    fn test_cache_clear() {
        let cache = LruEmbeddingCache::with_defaults();

        cache.set("key1", vec![1.0, 2.0], None).unwrap();
        cache.set("key2", vec![3.0, 4.0], None).unwrap();

        assert_eq!(cache.len(), 2);
        assert!(cache.size_bytes() > 0);

        cache.clear().unwrap();

        assert_eq!(cache.len(), 0);
        assert_eq!(cache.size_bytes(), 0);
    }

    #[test]
    fn test_cache_lru_eviction() {
        // Create a small cache (32 bytes max)
        let cache = LruEmbeddingCache::with_max_size(32);

        // Each f32 is 4 bytes, so 8 floats = 32 bytes
        let embedding1 = vec![1.0, 2.0, 3.0, 4.0]; // 16 bytes
        let embedding2 = vec![5.0, 6.0, 7.0, 8.0]; // 16 bytes
        let embedding3 = vec![9.0, 10.0, 11.0, 12.0]; // 16 bytes

        cache.set("key1", embedding1.clone(), None).unwrap();
        cache.set("key2", embedding2.clone(), None).unwrap();

        // Both should fit (32 bytes total)
        assert!(cache.get("key1").is_some());
        assert!(cache.get("key2").is_some());

        // Adding a third should evict the LRU (key1, since key2 was accessed more recently)
        cache.set("key3", embedding3.clone(), None).unwrap();

        // key1 should be evicted
        assert!(cache.get("key1").is_none());
        // key2 and key3 should exist
        assert!(cache.get("key2").is_some());
        assert!(cache.get("key3").is_some());

        assert!(cache.stats().evictions > 0);
    }

    #[test]
    fn test_cache_ttl_expiry() {
        let cache = LruEmbeddingCache::with_defaults();
        let key = "test_key";
        let embedding = vec![1.0, 2.0, 3.0];

        // Set with 0 duration TTL (immediate expiry)
        cache
            .set(key, embedding, Some(Duration::from_nanos(1)))
            .unwrap();

        // Sleep briefly to ensure expiry
        std::thread::sleep(Duration::from_millis(1));

        // Should be expired
        assert!(cache.get(key).is_none());
    }

    #[test]
    fn test_cache_stats() {
        let cache = LruEmbeddingCache::with_defaults();

        // Generate some activity
        cache.set("key1", vec![1.0, 2.0], None).unwrap();
        let _ = cache.get("key1"); // hit
        let _ = cache.get("key2"); // miss
        let _ = cache.get("key3"); // miss

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 2);
        assert_eq!(stats.entry_count, 1);
        assert!(stats.size_bytes > 0);
    }

    #[test]
    fn test_cache_hit_rate() {
        let stats = CacheStats {
            hits: 75,
            misses: 25,
            size_bytes: 0,
            entry_count: 0,
            evictions: 0,
        };

        assert!((stats.hit_rate() - 75.0).abs() < 0.001);
    }

    #[test]
    fn test_noop_cache() {
        let cache = NoOpCache::new();

        // Set should succeed but not store
        cache.set("key", vec![1.0, 2.0], None).unwrap();

        // Get should always return None
        assert!(cache.get("key").is_none());

        // Stats should be empty
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
        assert!(!cache.is_enabled());
    }

    #[test]
    fn test_cache_disabled() {
        let cache = LruEmbeddingCache::new(CacheConfig {
            enabled: false,
            ..Default::default()
        });

        // Set should succeed but not store
        cache.set("key", vec![1.0, 2.0], None).unwrap();

        // Get should return None when disabled
        assert!(cache.get("key").is_none());
        assert!(!cache.is_enabled());
    }

    #[test]
    fn test_cache_update_existing() {
        let cache = LruEmbeddingCache::with_defaults();
        let key = "test_key";

        cache.set(key, vec![1.0, 2.0], None).unwrap();
        let size1 = cache.size_bytes();

        // Update with different embedding
        cache.set(key, vec![3.0, 4.0, 5.0, 6.0], None).unwrap();
        let size2 = cache.size_bytes();

        // Size should have changed (old removed, new added)
        assert!(size2 > size1);
        assert_eq!(cache.len(), 1);

        // Should get the new value
        let retrieved = cache.get(key).unwrap();
        assert_eq!(retrieved, vec![3.0, 4.0, 5.0, 6.0]);
    }
}
