//! Error types for ares-vector.

use thiserror::Error;

/// Result type for ares-vector operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur in ares-vector operations.
#[derive(Error, Debug)]
pub enum Error {
    /// Collection already exists.
    #[error("Collection '{0}' already exists")]
    CollectionExists(String),

    /// Collection not found.
    #[error("Collection '{0}' not found")]
    CollectionNotFound(String),

    /// Vector not found.
    #[error("Vector '{0}' not found")]
    VectorNotFound(String),

    /// Dimension mismatch between vector and collection.
    #[error("Dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch {
        /// Expected dimensions.
        expected: usize,
        /// Actual dimensions provided.
        actual: usize,
    },

    /// Invalid vector (e.g., empty, contains NaN).
    #[error("Invalid vector: {0}")]
    InvalidVector(String),

    /// Index error during HNSW operations.
    #[error("Index error: {0}")]
    Index(String),

    /// Persistence error (I/O, serialization, etc.).
    #[error("Persistence error: {0}")]
    Persistence(String),

    /// Configuration error.
    #[error("Configuration error: {0}")]
    Configuration(String),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Internal error.
    #[error("Internal error: {0}")]
    Internal(String),
}
