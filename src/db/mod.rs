//! Database clients (Turso/SQLite, Qdrant).

#![allow(missing_docs)]

#[cfg(feature = "qdrant")]
pub mod qdrant;
pub mod traits;
pub mod turso;

#[cfg(feature = "qdrant")]
pub use qdrant::QdrantClient;
pub use turso::TursoClient;
