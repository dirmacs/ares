//! Common test utilities and mock implementations.
//!
//! This module provides shared test infrastructure to avoid duplication across test files.

pub mod mocks;
#[allow(dead_code)] // Shared DB harness: only the DB-backed test binaries call it; every binary compiles it via `mod common`.
pub mod test_db;
