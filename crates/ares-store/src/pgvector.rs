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
use crate::postgres::parse_postgres_url;

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

/// Distance metric for pgvector indexes and similarity queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PgVectorDistance {
    #[default]
    Cosine,
    L2,
    InnerProduct,
}

/// Default HNSW `m` (max edges per node).
pub const DEFAULT_HNSW_M: u16 = 16;

/// Default HNSW `ef_construction` build-time search depth.
pub const DEFAULT_HNSW_EF_CONSTRUCTION: u32 = 64;

/// HNSW index tuning parameters (`m`, `ef_construction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HnswIndexParams {
    pub m: u16,
    pub ef_construction: u32,
}

impl Default for HnswIndexParams {
    fn default() -> Self {
        Self {
            m: DEFAULT_HNSW_M,
            ef_construction: DEFAULT_HNSW_EF_CONSTRUCTION,
        }
    }
}

/// Returns the default HNSW `m`.
pub fn default_hnsw_m() -> u16 {
    DEFAULT_HNSW_M
}

/// Returns the default HNSW `ef_construction`.
pub fn default_hnsw_ef_construction() -> u32 {
    DEFAULT_HNSW_EF_CONSTRUCTION
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
    #[serde(default)]
    pub distance: PgVectorDistance,
    #[serde(default = "default_hnsw_m")]
    pub hnsw_m: u16,
    #[serde(default = "default_hnsw_ef_construction")]
    pub hnsw_ef_construction: u32,
}

impl Default for PgVectorConfig {
    fn default() -> Self {
        Self {
            connection_string: default_pgvector_url(),
            dimensions: default_vector_dimensions(),
            table_prefix: default_table_prefix(),
            index_type: PgVectorIndexType::default(),
            distance: PgVectorDistance::default(),
            hnsw_m: default_hnsw_m(),
            hnsw_ef_construction: default_hnsw_ef_construction(),
        }
    }
}

/// pgvector distance expression between the embedding column and query vector `$1`.
pub fn distance_expression(distance: PgVectorDistance) -> &'static str {
    match distance {
        PgVectorDistance::Cosine => "embedding <=> $1::vector",
        PgVectorDistance::L2 => "embedding <-> $1::vector",
        PgVectorDistance::InnerProduct => "embedding <#> $1::vector",
    }
}

/// pgvector opclass name used when creating indexes.
pub fn distance_ops_class(distance: PgVectorDistance) -> &'static str {
    match distance {
        PgVectorDistance::Cosine => "vector_cosine_ops",
        PgVectorDistance::L2 => "vector_l2_ops",
        PgVectorDistance::InnerProduct => "vector_ip_ops",
    }
}

/// Formats HNSW index `WITH` clause parameters for PostgreSQL.
pub fn hnsw_param_string(params: HnswIndexParams) -> String {
    format!(
        "m = {}, ef_construction = {}",
        params.m, params.ef_construction
    )
}

/// Builds [`HnswIndexParams`] from store configuration.
pub fn hnsw_index_params_from_config(config: &PgVectorConfig) -> HnswIndexParams {
    HnswIndexParams {
        m: config.hnsw_m,
        ef_construction: config.hnsw_ef_construction,
    }
}

/// Validates dimensions, identifiers, HNSW tuning, and the connection URL.
pub fn validate_index_config(config: &PgVectorConfig) -> std::result::Result<(), String> {
    if config.dimensions == 0 {
        return Err("dimensions must be greater than zero".to_string());
    }
    if config.connection_string.trim().is_empty() {
        return Err("connection_string must not be empty".to_string());
    }
    validate_collection_name(&config.table_prefix)?;
    if config.index_type == PgVectorIndexType::Hnsw {
        if config.hnsw_m < 2 {
            return Err("hnsw m must be at least 2".to_string());
        }
        if config.hnsw_ef_construction == 0 {
            return Err("hnsw ef_construction must be greater than zero".to_string());
        }
    }
    parse_pgvector_url(&config.connection_string)
        .map_err(|e| format!("invalid connection url: {e}"))?;
    Ok(())
}

/// Parses a PostgreSQL connection URL for pgvector storage.
#[cfg(feature = "postgres")]
pub fn parse_pgvector_url(
    url: &str,
) -> std::result::Result<crate::postgres::PostgresUrlParts, String> {
    parse_postgres_url(url)
}

/// Parses a PostgreSQL connection URL for pgvector storage (scheme check without `postgres` feature).
#[cfg(not(feature = "postgres"))]
pub fn parse_pgvector_url(url: &str) -> std::result::Result<(), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("empty url".to_string());
    }
    if !trimmed.starts_with("postgres://") && !trimmed.starts_with("postgresql://") {
        return Err("url must use postgres:// or postgresql:// scheme".to_string());
    }
    Ok(())
}

/// Returns `true` when `url` is a valid PostgreSQL connection string for pgvector.
pub fn is_pgvector_url(url: &str) -> bool {
    parse_pgvector_url(url).is_ok()
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
        return Err(
            "collection name may only contain ASCII letters, digits, and underscores".to_string(),
        );
    }
    Ok(())
}

/// Builds a fully qualified table name for a collection.
pub fn collection_table_name(
    prefix: &str,
    collection: &str,
) -> std::result::Result<String, String> {
    validate_collection_name(collection)?;
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return Err("table prefix must not be empty".to_string());
    }
    validate_collection_name(prefix)?;
    Ok(format!("{prefix}_{collection}"))
}

/// SQL to create a collection table with a `vector` column and JSONB metadata.
pub fn build_create_collection_sql(
    table: &str,
    dimensions: usize,
) -> std::result::Result<String, String> {
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

/// SQL to create an index on the embedding column (cosine distance, default HNSW params).
pub fn build_create_index_sql(
    table: &str,
    index_type: PgVectorIndexType,
) -> std::result::Result<String, String> {
    build_create_index_sql_with_distance(table, index_type, PgVectorDistance::Cosine, None)
}

/// SQL to create an index with an explicit distance metric and optional HNSW tuning.
pub fn build_create_index_sql_with_distance(
    table: &str,
    index_type: PgVectorIndexType,
    distance: PgVectorDistance,
    hnsw: Option<HnswIndexParams>,
) -> std::result::Result<String, String> {
    validate_collection_name(table)?;
    let ops = distance_ops_class(distance);
    let index_name = format!("{table}_embedding_idx");
    let stmt = match index_type {
        PgVectorIndexType::Hnsw => {
            let with_clause = hnsw_param_string(hnsw.unwrap_or_default());
            format!(
                "CREATE INDEX IF NOT EXISTS {index_name} ON {table} \
                 USING hnsw (embedding {ops}) WITH ({with_clause})"
            )
        }
        PgVectorIndexType::Ivfflat => format!(
            "CREATE INDEX IF NOT EXISTS {index_name} ON {table} \
             USING ivfflat (embedding {ops}) WITH (lists = 100)"
        ),
        PgVectorIndexType::None => return Ok(String::new()),
    };
    Ok(stmt)
}

/// SQL to create an index using [`PgVectorConfig`] distance and HNSW settings.
pub fn build_create_index_sql_for_config(
    table: &str,
    config: &PgVectorConfig,
) -> std::result::Result<String, String> {
    validate_index_config(config)?;
    let hnsw = (config.index_type == PgVectorIndexType::Hnsw)
        .then(|| hnsw_index_params_from_config(config));
    build_create_index_sql_with_distance(table, config.index_type, config.distance, hnsw)
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

/// Similarity search query for a collection table and distance metric.
pub fn build_search_query(
    table: &str,
    limit: usize,
    threshold: f32,
    distance: PgVectorDistance,
) -> std::result::Result<String, String> {
    validate_collection_name(table)?;
    if limit == 0 {
        return Err("limit must be greater than zero".to_string());
    }
    let dist = distance_expression(distance);
    let sql = match distance {
        PgVectorDistance::Cosine => format!(
            "SELECT id, content, metadata, \
             1 - ({dist}) AS score \
             FROM {table} \
             WHERE 1 - ({dist}) >= {threshold} \
             ORDER BY {dist} \
             LIMIT {limit}"
        ),
        PgVectorDistance::L2 | PgVectorDistance::InnerProduct => format!(
            "SELECT id, content, metadata, \
             {dist} AS distance \
             FROM {table} \
             WHERE {dist} <= {threshold} \
             ORDER BY {dist} \
             LIMIT {limit}"
        ),
    };
    Ok(sql)
}

/// Similarity search ordered by cosine distance (lower distance is more similar).
pub fn build_search_sql(
    table: &str,
    limit: usize,
    threshold: f32,
) -> std::result::Result<String, String> {
    build_search_query(table, limit, threshold, PgVectorDistance::Cosine)
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
    parse_pgvector_url(trimmed)
        .map_err(|e| AppError::Configuration(format!("invalid pgvector connection url: {e}")))?;
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
    use crate::postgres::is_postgres_url;
    use ares_types::types::AppError;

    // ── Connection URL resolution ────────────────────────────────────────

    #[test]
    fn default_pgvector_url_matches_constant() {
        assert_eq!(default_pgvector_url(), DEFAULT_PGVECTOR_URL);
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn parse_pgvector_url_delegates_to_postgres_parser() {
        let parts = parse_pgvector_url(DEFAULT_PGVECTOR_URL).expect("parse default url");
        assert_eq!(parts.database, "ares");
        assert!(is_pgvector_url(DEFAULT_PGVECTOR_URL));
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
        assert!(hnsw.contains("m = 16, ef_construction = 64"));

        let ivf = build_create_index_sql("ares_vec_docs", PgVectorIndexType::Ivfflat).expect("ivf");
        assert!(ivf.contains("USING ivfflat"));
        assert!(ivf.contains("lists = 100"));

        assert!(
            build_create_index_sql("ares_vec_docs", PgVectorIndexType::None)
                .expect("none")
                .is_empty()
        );
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
        assert_eq!(config.distance, PgVectorDistance::Cosine);
        assert_eq!(config.hnsw_m, DEFAULT_HNSW_M);
        assert_eq!(config.hnsw_ef_construction, DEFAULT_HNSW_EF_CONSTRUCTION);
    }

    #[test]
    fn pgvector_config_serde_roundtrip() {
        let config = PgVectorConfig {
            connection_string: "postgres://user:pass@host/vectors".into(),
            dimensions: 1536,
            table_prefix: "my_prefix".into(),
            index_type: PgVectorIndexType::Ivfflat,
            distance: PgVectorDistance::L2,
            hnsw_m: 32,
            hnsw_ef_construction: 128,
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

    // ── Pure helpers: distance / HNSW / index config ─────────────────────

    #[test]
    fn distance_expression_cosine_l2_and_inner_product() {
        assert_eq!(
            distance_expression(PgVectorDistance::Cosine),
            "embedding <=> $1::vector"
        );
        assert_eq!(
            distance_expression(PgVectorDistance::L2),
            "embedding <-> $1::vector"
        );
        assert_eq!(
            distance_expression(PgVectorDistance::InnerProduct),
            "embedding <#> $1::vector"
        );
    }

    #[test]
    fn distance_ops_class_matches_metric() {
        assert_eq!(
            distance_ops_class(PgVectorDistance::Cosine),
            "vector_cosine_ops"
        );
        assert_eq!(distance_ops_class(PgVectorDistance::L2), "vector_l2_ops");
        assert_eq!(
            distance_ops_class(PgVectorDistance::InnerProduct),
            "vector_ip_ops"
        );
    }

    #[test]
    fn hnsw_param_string_formats_m_and_ef_construction() {
        let s = hnsw_param_string(HnswIndexParams {
            m: 24,
            ef_construction: 200,
        });
        assert_eq!(s, "m = 24, ef_construction = 200");
    }

    #[test]
    fn hnsw_index_params_from_config_reads_fields() {
        let config = PgVectorConfig {
            hnsw_m: 12,
            hnsw_ef_construction: 80,
            ..PgVectorConfig::default()
        };
        let params = hnsw_index_params_from_config(&config);
        assert_eq!(params.m, 12);
        assert_eq!(params.ef_construction, 80);
    }

    #[test]
    fn validate_index_config_accepts_default() {
        assert!(validate_index_config(&PgVectorConfig::default()).is_ok());
    }

    #[test]
    fn validate_index_config_rejects_zero_dimensions() {
        let config = PgVectorConfig {
            dimensions: 0,
            ..PgVectorConfig::default()
        };
        let err = validate_index_config(&config).unwrap_err();
        assert!(err.contains("dimensions"));
    }

    #[test]
    fn validate_index_config_rejects_empty_connection() {
        let config = PgVectorConfig {
            connection_string: "   ".into(),
            ..PgVectorConfig::default()
        };
        let err = validate_index_config(&config).unwrap_err();
        assert!(err.contains("connection_string"));
    }

    #[test]
    fn validate_index_config_rejects_invalid_hnsw_m() {
        let config = PgVectorConfig {
            hnsw_m: 1,
            ..PgVectorConfig::default()
        };
        let err = validate_index_config(&config).unwrap_err();
        assert!(err.contains("hnsw m"));
    }

    #[test]
    fn validate_index_config_rejects_zero_ef_construction() {
        let config = PgVectorConfig {
            hnsw_ef_construction: 0,
            ..PgVectorConfig::default()
        };
        let err = validate_index_config(&config).unwrap_err();
        assert!(err.contains("ef_construction"));
    }

    #[test]
    fn validate_index_config_rejects_malformed_url() {
        let config = PgVectorConfig {
            connection_string: "mysql://localhost/db".into(),
            ..PgVectorConfig::default()
        };
        let err = validate_index_config(&config).unwrap_err();
        assert!(err.contains("invalid connection url"));
    }

    #[test]
    fn is_pgvector_url_rejects_garbage() {
        assert!(!is_pgvector_url("not-a-url"));
        assert!(!is_pgvector_url(""));
    }

    #[test]
    fn parse_pgvector_url_rejects_malformed_scheme() {
        let err = parse_pgvector_url("http://localhost/db").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn build_search_query_cosine_matches_legacy_search_sql() {
        let legacy = build_search_sql("ares_vec_docs", 5, 0.5).expect("legacy");
        let query =
            build_search_query("ares_vec_docs", 5, 0.5, PgVectorDistance::Cosine).expect("query");
        assert_eq!(legacy, query);
    }

    #[test]
    fn build_search_query_l2_uses_l2_operator_and_order() {
        let sql = build_search_query("ares_vec_docs", 8, 1.5, PgVectorDistance::L2).expect("l2");
        assert!(sql.contains("embedding <-> $1::vector"));
        assert!(sql.contains("ORDER BY embedding <-> $1::vector"));
        assert!(sql.contains("LIMIT 8"));
        assert!(sql.contains("<= 1.5"));
    }

    #[test]
    fn build_search_query_inner_product_uses_ip_operator() {
        let sql = build_search_query("ares_vec_docs", 3, 0.25, PgVectorDistance::InnerProduct)
            .expect("ip");
        assert!(sql.contains("embedding <#> $1::vector"));
        assert!(sql.contains("LIMIT 3"));
    }

    #[test]
    fn build_search_query_rejects_invalid_table_name() {
        assert!(build_search_query("bad-name", 1, 0.1, PgVectorDistance::Cosine).is_err());
    }

    #[test]
    fn build_create_index_sql_with_distance_uses_l2_ops() {
        let sql = build_create_index_sql_with_distance(
            "ares_vec_docs",
            PgVectorIndexType::Hnsw,
            PgVectorDistance::L2,
            Some(HnswIndexParams {
                m: 8,
                ef_construction: 40,
            }),
        )
        .expect("sql");
        assert!(sql.contains("vector_l2_ops"));
        assert!(sql.contains("m = 8, ef_construction = 40"));
    }

    #[test]
    fn build_create_index_sql_for_config_applies_distance_and_hnsw() {
        let config = PgVectorConfig {
            distance: PgVectorDistance::InnerProduct,
            hnsw_m: 20,
            hnsw_ef_construction: 100,
            ..PgVectorConfig::default()
        };
        let sql = build_create_index_sql_for_config("ares_vec_docs", &config).expect("sql");
        assert!(sql.contains("vector_ip_ops"));
        assert!(sql.contains("m = 20, ef_construction = 100"));
    }

    #[test]
    fn build_create_index_sql_for_config_rejects_bad_config() {
        let config = PgVectorConfig {
            dimensions: 0,
            ..PgVectorConfig::default()
        };
        assert!(build_create_index_sql_for_config("ares_vec_docs", &config).is_err());
    }

    #[test]
    fn pgvector_distance_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&PgVectorDistance::InnerProduct).expect("serialize"),
            r#""inner_product""#
        );
        let restored: PgVectorDistance = serde_json::from_str(r#""l2""#).expect("deserialize");
        assert_eq!(restored, PgVectorDistance::L2);
    }

    #[test]
    fn pgvector_config_serde_includes_distance_and_hnsw_fields() {
        let json = serde_json::to_string(&PgVectorConfig {
            distance: PgVectorDistance::L2,
            hnsw_m: 10,
            hnsw_ef_construction: 50,
            ..PgVectorConfig::default()
        })
        .expect("serialize");
        assert!(json.contains(r#""distance":"l2""#));
        assert!(json.contains(r#""hnsw_m":10"#));
        let restored: PgVectorConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.distance, PgVectorDistance::L2);
        assert_eq!(restored.hnsw_m, 10);
    }

    #[test]
    fn pgvector_config_deserializes_partial_json_with_distance_default() {
        let json =
            r#"{"connection_string":"postgres://custom/vectors","distance":"inner_product"}"#;
        let config: PgVectorConfig = serde_json::from_str(json).expect("deserialize");
        assert_eq!(config.distance, PgVectorDistance::InnerProduct);
        assert_eq!(config.dimensions, DEFAULT_VECTOR_DIMENSIONS);
    }

    #[test]
    fn hnsw_index_params_default_matches_constants() {
        let params = HnswIndexParams::default();
        assert_eq!(params.m, DEFAULT_HNSW_M);
        assert_eq!(params.ef_construction, DEFAULT_HNSW_EF_CONSTRUCTION);
    }

    #[test]
    fn build_create_index_ivfflat_with_inner_product_ops() {
        let sql = build_create_index_sql_with_distance(
            "ares_vec_docs",
            PgVectorIndexType::Ivfflat,
            PgVectorDistance::InnerProduct,
            None,
        )
        .expect("sql");
        assert!(sql.contains("vector_ip_ops"));
        assert!(sql.contains("lists = 100"));
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn test_pgvector_url_parses_database_name() {
        let parts = parse_pgvector_url(TEST_PGVECTOR_URL).expect("parse test url");
        assert_eq!(parts.database, "test");
        assert!(is_pgvector_url(TEST_PGVECTOR_URL));
    }

    #[tokio::test]
    async fn new_returns_configuration_error_when_not_implemented() {
        let err = PgVectorStore::new(DEFAULT_PGVECTOR_URL).await.unwrap_err();
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
        let err = PgVectorStore::new(DEFAULT_PGVECTOR_URL).await.unwrap_err();
        let msg = match err {
            AppError::Configuration(m) => m,
            other => panic!("expected Configuration, got {other:?}"),
        };
        assert!(msg.contains("qdrant"), "should suggest qdrant: {msg}");
    }
}
