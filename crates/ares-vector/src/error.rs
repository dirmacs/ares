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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error as IoError, ErrorKind};

    #[test]
    fn collection_exists_display() {
        let err = Error::CollectionExists("docs".into());
        assert_eq!(err.to_string(), "Collection 'docs' already exists");
    }

    #[test]
    fn collection_not_found_display() {
        let err = Error::CollectionNotFound("missing".into());
        assert_eq!(err.to_string(), "Collection 'missing' not found");
    }

    #[test]
    fn vector_not_found_display() {
        let err = Error::VectorNotFound("vec-42".into());
        assert_eq!(err.to_string(), "Vector 'vec-42' not found");
    }

    #[test]
    fn dimension_mismatch_display() {
        let err = Error::DimensionMismatch {
            expected: 384,
            actual: 128,
        };
        assert_eq!(
            err.to_string(),
            "Dimension mismatch: expected 384, got 128"
        );
    }

    #[test]
    fn invalid_vector_display() {
        let err = Error::InvalidVector("contains NaN".into());
        assert_eq!(err.to_string(), "Invalid vector: contains NaN");
    }

    #[test]
    fn index_display() {
        let err = Error::Index("HNSW build failed".into());
        assert_eq!(err.to_string(), "Index error: HNSW build failed");
    }

    #[test]
    fn persistence_display() {
        let err = Error::Persistence("disk full".into());
        assert_eq!(err.to_string(), "Persistence error: disk full");
    }

    #[test]
    fn configuration_display() {
        let err = Error::Configuration("bad metric".into());
        assert_eq!(err.to_string(), "Configuration error: bad metric");
    }

    #[test]
    fn internal_display() {
        let err = Error::Internal("unexpected state".into());
        assert_eq!(err.to_string(), "Internal error: unexpected state");
    }

    #[test]
    fn io_error_from_std_io_error() {
        let io_err = IoError::new(ErrorKind::NotFound, "no such file");
        let err: Error = io_err.into();
        assert!(matches!(err, Error::Io(_)));
        assert!(err.to_string().contains("no such file"));
    }

    #[test]
    fn result_type_alias_accepts_ok() {
        let ok: Result<i32> = Ok(7);
        assert_eq!(ok.unwrap(), 7);
    }

    #[test]
    fn result_type_alias_accepts_err() {
        let err: Result<()> = Err(Error::CollectionNotFound("x".into()));
        assert!(matches!(err, Err(Error::CollectionNotFound(name)) if name == "x"));
    }

    #[test]
    fn debug_includes_variant_name() {
        let err = Error::DimensionMismatch {
            expected: 3,
            actual: 2,
        };
        let dbg = format!("{err:?}");
        assert!(dbg.contains("DimensionMismatch"));
        assert!(dbg.contains("expected: 3"));
        assert!(dbg.contains("actual: 2"));
    }
}
