//! Configuration for ares-vector.

use std::path::PathBuf;

/// Configuration for the vector database.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Config {
    /// Path to store data on disk. If None, data is kept in memory only.
    pub data_path: Option<PathBuf>,

    /// HNSW index configuration.
    pub hnsw_config: HnswConfig,

    /// Maximum number of vectors per collection (0 = unlimited).
    pub max_vectors: usize,

    /// Enable automatic persistence (periodic snapshots).
    pub auto_persist: bool,

    /// Interval for automatic persistence in seconds.
    pub persist_interval_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_path: None,
            hnsw_config: HnswConfig::default(),
            max_vectors: 0,
            auto_persist: false,
            persist_interval_secs: 300,
        }
    }
}

impl Config {
    /// Create an in-memory configuration.
    ///
    /// Data will not be persisted and will be lost when the process exits.
    pub fn memory() -> Self {
        Self::default()
    }

    /// Create a persistent configuration.
    ///
    /// Data will be stored at the specified path and loaded on startup.
    pub fn persistent<P: Into<PathBuf>>(path: P) -> Self {
        Self {
            data_path: Some(path.into()),
            auto_persist: true,
            ..Self::default()
        }
    }

    /// Set the HNSW configuration.
    pub fn with_hnsw_config(mut self, config: HnswConfig) -> Self {
        self.hnsw_config = config;
        self
    }

    /// Set the maximum number of vectors per collection.
    pub fn with_max_vectors(mut self, max: usize) -> Self {
        self.max_vectors = max;
        self
    }

    /// Enable or disable automatic persistence.
    pub fn with_auto_persist(mut self, enabled: bool) -> Self {
        self.auto_persist = enabled;
        self
    }

    /// Set the persistence interval in seconds.
    pub fn with_persist_interval(mut self, secs: u64) -> Self {
        self.persist_interval_secs = secs;
        self
    }
}

/// HNSW index configuration.
///
/// These parameters control the trade-off between search accuracy,
/// speed, and memory usage.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HnswConfig {
    /// Maximum number of connections per element per layer.
    ///
    /// Higher values improve search quality but use more memory.
    /// Typical values: 12-48. Default: 16.
    pub m: usize,

    /// Maximum number of connections for the first layer.
    ///
    /// Usually set to 2 * M. Default: 32.
    pub m_max: usize,

    /// Size of the dynamic candidate list during construction.
    ///
    /// Higher values improve index quality but slow down construction.
    /// Typical values: 100-500. Default: 200.
    pub ef_construction: usize,

    /// Size of the dynamic candidate list during search.
    ///
    /// Higher values improve search quality but slow down search.
    /// Must be >= k (number of results). Default: 100.
    pub ef_search: usize,

    /// Enable parallel index construction.
    ///
    /// Uses multiple threads to speed up batch insertions.
    pub parallel_construction: bool,

    /// Number of threads for parallel operations (0 = auto).
    pub num_threads: usize,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            m: 16,
            m_max: 32,
            ef_construction: 200,
            ef_search: 100,
            parallel_construction: true,
            num_threads: 0, // Auto-detect
        }
    }
}

impl HnswConfig {
    /// Create a configuration optimized for speed.
    ///
    /// Lower accuracy but faster search and construction.
    pub fn fast() -> Self {
        Self {
            m: 8,
            m_max: 16,
            ef_construction: 100,
            ef_search: 50,
            parallel_construction: true,
            num_threads: 0,
        }
    }

    /// Create a configuration optimized for accuracy.
    ///
    /// Higher accuracy but slower search and uses more memory.
    pub fn accurate() -> Self {
        Self {
            m: 32,
            m_max: 64,
            ef_construction: 400,
            ef_search: 200,
            parallel_construction: true,
            num_threads: 0,
        }
    }

    /// Create a configuration optimized for memory efficiency.
    ///
    /// Lower memory usage but may have lower accuracy.
    pub fn memory_efficient() -> Self {
        Self {
            m: 8,
            m_max: 16,
            ef_construction: 100,
            ef_search: 64,
            parallel_construction: false,
            num_threads: 1,
        }
    }

    /// Set the M parameter (connections per layer).
    pub fn with_m(mut self, m: usize) -> Self {
        self.m = m;
        self.m_max = m * 2;
        self
    }

    /// Set the ef_construction parameter.
    pub fn with_ef_construction(mut self, ef: usize) -> Self {
        self.ef_construction = ef;
        self
    }

    /// Set the ef_search parameter.
    pub fn with_ef_search(mut self, ef: usize) -> Self {
        self.ef_search = ef;
        self
    }

    /// Set the number of threads for parallel operations.
    pub fn with_num_threads(mut self, n: usize) -> Self {
        self.num_threads = n;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_config() {
        let config = Config::memory();
        assert!(config.data_path.is_none());
        assert!(!config.auto_persist);
    }

    #[test]
    fn test_persistent_config() {
        let config = Config::persistent("/tmp/vectors");
        assert!(config.data_path.is_some());
        assert!(config.auto_persist);
    }

    #[test]
    fn test_hnsw_presets() {
        let fast = HnswConfig::fast();
        let accurate = HnswConfig::accurate();

        assert!(fast.m < accurate.m);
        assert!(fast.ef_construction < accurate.ef_construction);
    }
}
