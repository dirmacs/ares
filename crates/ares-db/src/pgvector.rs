//! PostgreSQL pgvector integration.
//!
//! This module provides vector similarity search using PostgreSQL with the pgvector extension.
//!
//! # Status
//!
//! **Not yet implemented.** Connection URL resolution, configuration, and SQL builders are
//! available for testing; live pool/search operations will be added in a future release.
//!
//! # Feature Flag
//!
//! Enable with `--features pgvector` (implies `postgres`) or `--features postgres` for helpers.

use ares_types::types::{AppError, Result};
use serde::{Deserialize, Serialize};

#[cfg(feature = "postgres")]
use crate::postgres::{is_postgres_url, parse_postgres_url};

/// Default PostgreSQL URL for pgvector-backed storage.
pub const DEFAULT_PGVECTOR_URL: &str = "postgres://postgres:postgres@localhost:5432/ares";

/// Test URL used by unit tests (no live connection required).
pub const TEST_PGVECTOR_URL: &str = "postgres://test:test@localhost:5432/test";

/// SQL to ensure the pgvector extension exists.
pub const CREATE_VECTOR_EXTENSION_SQL: &str = "CREATE EXTENSION IF NOT EXISTS vector";

/// Default embedding dimensions (BGE-small).
pub const DEFAULT_VECTOR_DIMENSIONS: usize = 384;

/// Default table prefix for per-collection tables.
pub const DEFAULT_TABLE_PREFIX: &str = "ares_vec";

/// Returns the default pgvector connection URL.
pub fn default_pgvector_url() -> String {
    DEFAULT_PGVECTOR_URL.to_string()
}

/// Returns the default vector dimensions.
pub fn default_vector_dimensions() -> usize {
    DEFAULT_VECTOR_DIMENSIONS
}

/// Returns the default table prefix.
pub fn default_table_prefix() -> String {
    DEFAULT_TABLE_PREFIX.to_string()
}

/// Resolve a pgvector database URL from an explicit override, `PGVECTOR_URL`, `DATABASE_URL`, or
/// [`default_pgvector_url`].
pub fn resolve_pgvector_url(override_url: Option<&str>) -> String {
    if let Some(url) = override_url {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    std::env::var("PGVECTOR_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| default_pgvector_url())
}

/// Index strategy for pgvector collections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PgVectorIndexType {
    #[default]
    Hnsw,
    Ivfflat,
    None,
}

/// Configuration for a pgvector-backed store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PgVectorConfig {
    #[serde(default = "default_pgvector_url")]
    pub connection_string: String,
    #[serde(default = "default_vector_dimensions")]
    pub dimensions: usize,
    #[serde(default = "default_table_prefix")]
    pub table_prefix: String,
    #[serde(default)]
    pub index_type: PgVectorIndexType,
}

impl Default for PgVectorConfig {
    fn default() -> Self {
        Self {
            connection_string: default_pgvector_url(),
            dimensions: default_vector_dimensions(),
            table_prefix: default_table_prefix(),
            index_type: PgVectorIndexType::default(),
        }
    }
}

/// Validates a collection name for use in SQL identifiers.
pub fn validate_collection_name(name: &str) -> std::result::Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("collection name must not be empty".to_string());
    }
    if !name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return Err("collection name must start with a letter or underscore".to_string());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("collection name may only contain ASCII letters, digits, and underscores".to_string());
    }
    Ok(())
}

/// Builds a fully qualified table name for a collection.
pub fn collection_table_name(prefix: &str, collection: &str) -> std::result::Result<String, String> {
    validate_collection_name(collection)?;
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return Err("table prefix must not be empty".to_string());
    }
    validate_collection_name(prefix)?;
    Ok(format!("{prefix}_{collection}"))
}

/// SQL to create a collection table with a `vector` column and JSONB metadata.
pub fn build_create_collection_sql(table: &str, dimensions: usize) -> std::result::Result<String, String> {
    validate_collection_name(table)?;
    if dimensions == 0 {
        return Err("dimensions must be greater than zero".to_string());
    }
    Ok(format!(
        "CREATE TABLE IF NOT EXISTS {table} ( \
            id TEXT PRIMARY KEY, \
            embedding vector({dimensions}), \
            content TEXT NOT NULL, \
            metadata JSONB NOT NULL DEFAULT '{{}}'::jsonb, \
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW() \
        )"
    ))
}

/// SQL to create an index on the embedding column.
pub fn build_create_index_sql(table: &str, index_type: PgVectorIndexType) -> std::result::Result<String, String> {
    validate_collection_name(table)?;
    let index_name = format!("{table}_embedding_idx");
    let stmt = match index_type {
        PgVectorIndexType::Hnsw => format!(
            "CREATE INDEX IF NOT EXISTS {index_name} ON {table} \
             USING hnsw (embedding vector_cosine_ops)"
        ),
        PgVectorIndexType::Ivfflat => format!(
            "CREATE INDEX IF NOT EXISTS {index_name} ON {table} \
             USING ivfflat (embedding vector_cosine_ops) WITH (lists = 100)"
        ),
        PgVectorIndexType::None => return Ok(String::new()),
    };
    Ok(stmt)
}

/// Parameterized upsert for a document row.
pub fn build_upsert_sql(table: &str) -> std::result::Result<String, String> {
    validate_collection_name(table)?;
    Ok(format!(
        "INSERT INTO {table} (id, embedding, content, metadata) \
         VALUES ($1, $2::vector, $3, $4::jsonb) \
         ON CONFLICT (id) DO UPDATE SET \
            embedding = EXCLUDED.embedding, \
            content = EXCLUDED.content, \
            metadata = EXCLUDED.metadata"
    ))
}

/// Similarity search ordered by cosine distance (lower is more similar).
pub fn build_search_sql(table: &str, limit: usize, threshold: f32) -> std::result::Result<String, String> {
    validate_collection_name(table)?;
    if limit == 0 {
        return Err("limit must be greater than zero".to_string());
    }
    Ok(format!(
        "SELECT id, content, metadata, \
         1 - (embedding <=> $1::vector) AS score \
         FROM {table} \
         WHERE 1 - (embedding <=> $1::vector) >= {threshold} \
         ORDER BY embedding <=> $1::vector \
         LIMIT {limit}"
    ))
}

/// Metadata filter fragment for JSONB key/value equality.
pub fn build_metadata_filter_sql(key: &str, value: &str) -> String {
    let key = key.replace('\'', "''");
    let value = value.replace('\'', "''");
    format!("metadata->>'{key}' = '{value}'")
}

/// PostgreSQL pgvector store (not yet implemented).
///
/// This struct will provide vector similarity search using PostgreSQL
/// with the pgvector extension.
#[derive(Debug)]
pub struct PgVectorStore {
    _private: (),
}

impl PgVectorStore {
    /// Create a new PgVectorStore after validating the connection URL.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Configuration`] when the URL is invalid or the store is not yet
    /// fully implemented.
    pub async fn new(connection_string: &str) -> Result<Self> {
        validate_connection_string(connection_string)?;
        Err(not_implemented_error())
    }

    /// Create from a [`PgVectorConfig`].
    pub async fn from_config(config: &PgVectorConfig) -> Result<Self> {
        Self::new(&config.connection_string).await
    }
}

fn validate_connection_string(connection_string: &str) -> Result<()> {
    let trimmed = connection_string.trim();
    if trimmed.is_empty() {
        return Err(AppError::Configuration(
            "empty pgvector connection url".to_string(),
        ));
    }
    #[cfg(feature = "postgres")]
    {
        parse_postgres_url(trimmed).map_err(|e| {
            AppError::Configuration(format!("invalid pgvector connection url: {e}"))
        })?;
    }
    #[cfg(not(feature = "postgres"))]
    {
        let _ = trimmed;
    }
    Ok(())
}

fn not_implemented_error() -> AppError {
    AppError::Configuration(
        "PgVectorStore is not yet implemented. Use 'ares-vector' (default) or 'qdrant' instead. \
         See https://github.com/dirmacs/ares for implementation status."
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::types::AppError;

    // ── Connection URL resolution ────────────────────────────────────────

    #[test]
    fn default_pgvector_url_matches_constant() {
        assert_eq!(default_pgvector_url(), DEFAULT_PGVECTOR_URL);
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn parse_pgvector_url_delegates_to_postgres_parser() {
        let parts = parse_postgres_url(DEFAULT_PGVECTOR_URL).expect("parse default url");
        assert_eq!(parts.database, "ares");
        assert!(is_postgres_url(DEFAULT_PGVECTOR_URL));
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn validate_connection_rejects_invalid_urls() {
        let err = validate_connection_string("mysql://localhost/db").unwrap_err();
        matches::assert_matches!(err, AppError::Configuration(msg) if msg.contains("invalid"));
    }

    #[test]
    fn validate_connection_rejects_empty_url() {
        let err = validate_connection_string("   ").unwrap_err();
        matches::assert_matches!(err, AppError::Configuration(msg) if msg.contains("empty"));
    }

    #[test]
    fn resolve_pgvector_url_prefers_explicit_override() {
        std::env::remove_var("PGVECTOR_URL");
        std::env::remove_var("DATABASE_URL");
        assert_eq!(
            resolve_pgvector_url(Some("postgres://override/vectors")),
            "postgres://override/vectors"
        );
    }

    #[test]
    fn resolve_pgvector_url_trims_override() {
        std::env::remove_var("PGVECTOR_URL");
        std::env::remove_var("DATABASE_URL");
        assert_eq!(
            resolve_pgvector_url(Some("  postgres://trimmed/db  ")),
            "postgres://trimmed/db"
        );
    }

    #[test]
    fn resolve_pgvector_url_falls_back_to_default_when_env_missing() {
        std::env::remove_var("PGVECTOR_URL");
        std::env::remove_var("DATABASE_URL");
        assert_eq!(resolve_pgvector_url(None), default_pgvector_url());
    }

    #[test]
    fn resolve_pgvector_url_ignores_blank_override() {
        std::env::remove_var("PGVECTOR_URL");
        std::env::remove_var("DATABASE_URL");
        assert_eq!(resolve_pgvector_url(Some("   ")), default_pgvector_url());
    }

    #[test]
    fn test_pgvector_url_is_valid_postgres_url() {
        #[cfg(feature = "postgres")]
        {
            let parts = parse_postgres_url(TEST_PGVECTOR_URL).expect("parse test url");
            assert_eq!(parts.database, "test");
        }
    }

    // ── Collection / table naming ────────────────────────────────────────

    #[test]
    fn validate_collection_name_accepts_safe_identifiers() {
        assert!(validate_collection_name("documents").is_ok());
        assert!(validate_collection_name("_private").is_ok());
    }

    #[test]
    fn validate_collection_name_rejects_invalid() {
        assert!(validate_collection_name("").is_err());
        assert!(validate_collection_name("1bad").is_err());
        assert!(validate_collection_name("has-dash").is_err());
    }

    #[test]
    fn collection_table_name_joins_prefix_and_collection() {
        let table = collection_table_name("ares_vec", "documents").expect("table name");
        assert_eq!(table, "ares_vec_documents");
    }

    // ── Query building ───────────────────────────────────────────────────

    #[test]
    fn create_vector_extension_sql_is_idempotent() {
        assert!(CREATE_VECTOR_EXTENSION_SQL.contains("IF NOT EXISTS"));
        assert!(CREATE_VECTOR_EXTENSION_SQL.contains("vector"));
    }

    #[test]
    fn build_create_collection_sql_includes_vector_column() {
        let sql = build_create_collection_sql("ares_vec_docs", 384).expect("sql");
        assert!(sql.contains("vector(384)"));
        assert!(sql.contains("metadata JSONB"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS ares_vec_docs"));
    }

    #[test]
    fn build_create_collection_sql_rejects_zero_dimensions() {
        assert!(build_create_collection_sql("docs", 0).is_err());
    }

    #[test]
    fn build_create_index_sql_for_hnsw_and_ivfflat() {
        let hnsw = build_create_index_sql("ares_vec_docs", PgVectorIndexType::Hnsw).expect("hnsw");
        assert!(hnsw.contains("USING hnsw"));
        assert!(hnsw.contains("vector_cosine_ops"));

        let ivf = build_create_index_sql("ares_vec_docs", PgVectorIndexType::Ivfflat).expect("ivf");
        assert!(ivf.contains("USING ivfflat"));
        assert!(ivf.contains("lists = 100"));

        assert!(build_create_index_sql("ares_vec_docs", PgVectorIndexType::None)
            .expect("none")
            .is_empty());
    }

    #[test]
    fn build_upsert_sql_uses_conflict_target() {
        let sql = build_upsert_sql("ares_vec_docs").expect("upsert");
        assert!(sql.contains("ON CONFLICT (id)"));
        assert!(sql.contains("$2::vector"));
    }

    #[test]
    fn build_search_sql_orders_by_distance_and_applies_threshold() {
        let sql = build_search_sql("ares_vec_docs", 10, 0.75).expect("search");
        assert!(sql.contains("<=> $1::vector"));
        assert!(sql.contains(">= 0.75"));
        assert!(sql.contains("LIMIT 10"));
    }

    #[test]
    fn build_search_sql_rejects_zero_limit() {
        assert!(build_search_sql("docs", 0, 0.5).is_err());
    }

    #[test]
    fn build_metadata_filter_sql_escapes_quotes() {
        let sql = build_metadata_filter_sql("source", "it's fine");
        assert!(sql.contains("metadata->>'source'"));
        assert!(sql.contains("it''s fine"));
    }

    // ── Serde: PgVectorConfig ────────────────────────────────────────────

    #[test]
    fn pgvector_config_default_values() {
        let config = PgVectorConfig::default();
        assert_eq!(config.connection_string, default_pgvector_url());
        assert_eq!(config.dimensions, DEFAULT_VECTOR_DIMENSIONS);
        assert_eq!(config.table_prefix, DEFAULT_TABLE_PREFIX);
        assert_eq!(config.index_type, PgVectorIndexType::Hnsw);
    }

    #[test]
    fn pgvector_config_serde_roundtrip() {
        let config = PgVectorConfig {
            connection_string: "postgres://user:pass@host/vectors".into(),
            dimensions: 1536,
            table_prefix: "my_prefix".into(),
            index_type: PgVectorIndexType::Ivfflat,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let restored: PgVectorConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, config);
    }

    #[test]
    fn pgvector_config_deserializes_with_defaults() {
        let json = r#"{"connection_string":"postgres://custom/vectors"}"#;
        let config: PgVectorConfig = serde_json::from_str(json).expect("deserialize");
        assert_eq!(config.connection_string, "postgres://custom/vectors");
        assert_eq!(config.dimensions, DEFAULT_VECTOR_DIMENSIONS);
        assert_eq!(config.index_type, PgVectorIndexType::Hnsw);
    }

    #[test]
    fn pgvector_index_type_serde_snake_case() {
        let json = serde_json::to_string(&PgVectorIndexType::Ivfflat).expect("serialize");
        assert_eq!(json, "\"ivfflat\"");
    }

    // ── Error handling: PgVectorStore::new ─────────────────────────────────

    #[tokio::test]
    async fn new_returns_configuration_error_when_not_implemented() {
        let err = PgVectorStore::new(DEFAULT_PGVECTOR_URL)
            .await
            .unwrap_err();
        matches::assert_matches!(err, AppError::Configuration(msg) if {
            msg.contains("not yet implemented") && msg.contains("ares-vector")
        });
    }

    #[tokio::test]
    async fn new_rejects_invalid_url_before_not_implemented() {
        let err = PgVectorStore::new("not-a-url").await.unwrap_err();
        matches::assert_matches!(err, AppError::Configuration(msg) if msg.contains("invalid"));
    }

    #[tokio::test]
    async fn new_rejects_empty_url() {
        let err = PgVectorStore::new("  ").await.unwrap_err();
        matches::assert_matches!(err, AppError::Configuration(msg) if msg.contains("empty"));
    }

    #[tokio::test]
    async fn from_config_validates_then_returns_not_implemented() {
        let config = PgVectorConfig {
            connection_string: DEFAULT_PGVECTOR_URL.to_string(),
            ..PgVectorConfig::default()
        };
        let err = PgVectorStore::from_config(&config).await.unwrap_err();
        matches::assert_matches!(err, AppError::Configuration(_));
    }

    #[tokio::test]
    async fn new_error_mentions_alternatives() {
        let err = PgVectorStore::new(DEFAULT_PGVECTOR_URL)
            .await
            .unwrap_err();
        let msg = match err {
            AppError::Configuration(m) => m,
            other => panic!("expected Configuration, got {other:?}"),
        };
        assert!(msg.contains("qdrant"), "should suggest qdrant: {msg}");
    }
}
