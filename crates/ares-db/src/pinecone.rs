//! Pinecone vector database integration.
//!
//! This module provides integration with Pinecone, a managed cloud vector database.
//!
//! # Status
//!
//! **Not yet implemented.** Connection resolution, configuration, and validation helpers are
//! available for testing; live index/search operations will be added in a future release.
//!
//! # Feature Flag
//!
//! Enable with `--features pinecone` or `--features postgres` for helpers.

use ares_types::types::{AppError, Result};
use serde::{Deserialize, Serialize};

/// Default Pinecone environment (cloud region).
pub const DEFAULT_PINECONE_ENVIRONMENT: &str = "us-east-1-aws";

/// Default embedding dimensions (BGE-small).
pub const DEFAULT_VECTOR_DIMENSIONS: usize = 384;

/// Returns the default Pinecone environment identifier.
pub fn default_pinecone_environment() -> String {
    DEFAULT_PINECONE_ENVIRONMENT.to_string()
}

/// Returns the default vector dimensions.
pub fn default_vector_dimensions() -> usize {
    DEFAULT_VECTOR_DIMENSIONS
}

/// Resolve Pinecone credentials from explicit overrides or environment variables.
pub fn resolve_pinecone_credentials(
    api_key_override: Option<&str>,
    environment_override: Option<&str>,
) -> (Option<String>, String) {
    let api_key = api_key_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::var("PINECONE_API_KEY").ok());

    let environment = environment_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::var("PINECONE_ENVIRONMENT").ok())
        .unwrap_or_else(default_pinecone_environment);

    (api_key, environment)
}

/// Configuration for a Pinecone-backed store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PineconeConfig {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_pinecone_environment")]
    pub environment: String,
    #[serde(default = "default_vector_dimensions")]
    pub dimensions: usize,
}

impl Default for PineconeConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            environment: default_pinecone_environment(),
            dimensions: default_vector_dimensions(),
        }
    }
}

/// Validates an index name for Pinecone namespaces/indexes.
pub fn validate_index_name(name: &str) -> std::result::Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("index name must not be empty".to_string());
    }
    if !name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return Err("index name must start with a letter or underscore".to_string());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(
            "index name may only contain ASCII letters, digits, underscores, and hyphens"
                .to_string(),
        );
    }
    Ok(())
}

/// Builds a Pinecone index host URL from environment and index name.
pub fn build_index_host(environment: &str, index_name: &str) -> std::result::Result<String, String> {
    validate_index_name(index_name)?;
    let environment = environment.trim();
    if environment.is_empty() {
        return Err("pinecone environment must not be empty".to_string());
    }
    Ok(format!("https://{index_name}-{environment}.svc.pinecone.io"))
}

/// Validates Pinecone connection settings before client construction.
pub fn validate_pinecone_config(config: &PineconeConfig) -> Result<()> {
    if config
        .api_key
        .as_deref()
        .is_none_or(|key| key.trim().is_empty())
    {
        return Err(AppError::Configuration(
            "pinecone api key is required".to_string(),
        ));
    }
    if config.environment.trim().is_empty() {
        return Err(AppError::Configuration(
            "pinecone environment must not be empty".to_string(),
        ));
    }
    if config.dimensions == 0 {
        return Err(AppError::Configuration(
            "pinecone dimensions must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

/// Pinecone vector store (not yet implemented).
///
/// This struct will provide integration with Pinecone's managed
/// vector database service.
#[derive(Debug)]
pub struct PineconeStore {
    _private: (),
}

impl PineconeStore {
    /// Create a new PineconeStore after validating credentials.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Configuration`] when credentials are invalid or the store is not yet
    /// fully implemented.
    pub async fn new(api_key: &str, environment: &str) -> Result<Self> {
        let config = PineconeConfig {
            api_key: Some(api_key.to_string()),
            environment: environment.to_string(),
            ..PineconeConfig::default()
        };
        Self::from_config(&config).await
    }

    /// Create from a [`PineconeConfig`].
    pub async fn from_config(config: &PineconeConfig) -> Result<Self> {
        validate_pinecone_config(config)?;
        Err(not_implemented_error())
    }
}

fn not_implemented_error() -> AppError {
    AppError::Configuration(
        "PineconeStore is not yet implemented. Use 'ares-vector' (default) or 'qdrant' instead. \
         See https://github.com/dirmacs/ares for implementation status."
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::types::AppError;

    // ── Connection logic ─────────────────────────────────────────────────

    #[test]
    fn resolve_pinecone_credentials_prefers_explicit_overrides() {
        std::env::remove_var("PINECONE_API_KEY");
        std::env::remove_var("PINECONE_ENVIRONMENT");
        let (api_key, environment) =
            resolve_pinecone_credentials(Some("  override-key  "), Some(" eu-west1 "));
        assert_eq!(api_key.as_deref(), Some("override-key"));
        assert_eq!(environment, "eu-west1");
    }

    #[test]
    fn resolve_pinecone_credentials_falls_back_to_defaults() {
        std::env::remove_var("PINECONE_API_KEY");
        std::env::remove_var("PINECONE_ENVIRONMENT");
        let (api_key, environment) = resolve_pinecone_credentials(None, None);
        assert!(api_key.is_none());
        assert_eq!(environment, default_pinecone_environment());
    }

    #[test]
    fn resolve_pinecone_credentials_ignores_blank_overrides() {
        std::env::remove_var("PINECONE_API_KEY");
        std::env::remove_var("PINECONE_ENVIRONMENT");
        let (api_key, environment) = resolve_pinecone_credentials(Some("   "), Some("   "));
        assert!(api_key.is_none());
        assert_eq!(environment, default_pinecone_environment());
    }

    #[test]
    fn build_index_host_formats_expected_url() {
        let host = build_index_host("us-east-1-aws", "documents").expect("host");
        assert_eq!(
            host,
            "https://documents-us-east-1-aws.svc.pinecone.io"
        );
    }

    #[test]
    fn build_index_host_rejects_invalid_index_name() {
        assert!(build_index_host("us-east-1-aws", "").is_err());
        assert!(build_index_host("us-east-1-aws", "1bad").is_err());
    }

    // ── Query / index naming ─────────────────────────────────────────────

    #[test]
    fn validate_index_name_accepts_safe_identifiers() {
        assert!(validate_index_name("documents").is_ok());
        assert!(validate_index_name("tenant_docs").is_ok());
        assert!(validate_index_name("my-index").is_ok());
    }

    #[test]
    fn validate_index_name_rejects_invalid() {
        assert!(validate_index_name("").is_err());
        assert!(validate_index_name("1bad").is_err());
        assert!(validate_index_name("has space").is_err());
    }

    // ── Serde: PineconeConfig ────────────────────────────────────────────

    #[test]
    fn pinecone_config_default_values() {
        let config = PineconeConfig::default();
        assert!(config.api_key.is_none());
        assert_eq!(config.environment, default_pinecone_environment());
        assert_eq!(config.dimensions, DEFAULT_VECTOR_DIMENSIONS);
    }

    #[test]
    fn pinecone_config_serde_roundtrip() {
        let config = PineconeConfig {
            api_key: Some("test-key".into()),
            environment: "gcp-starter".into(),
            dimensions: 1536,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let restored: PineconeConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, config);
    }

    #[test]
    fn pinecone_config_deserializes_with_defaults() {
        let json = r#"{"api_key":"abc123"}"#;
        let config: PineconeConfig = serde_json::from_str(json).expect("deserialize");
        assert_eq!(config.api_key.as_deref(), Some("abc123"));
        assert_eq!(config.environment, default_pinecone_environment());
        assert_eq!(config.dimensions, DEFAULT_VECTOR_DIMENSIONS);
    }

    // ── Error handling ───────────────────────────────────────────────────

    #[test]
    fn validate_pinecone_config_rejects_missing_api_key() {
        let config = PineconeConfig {
            api_key: None,
            ..PineconeConfig::default()
        };
        let err = validate_pinecone_config(&config).unwrap_err();
        matches::assert_matches!(err, AppError::Configuration(msg) if msg.contains("api key"));
    }

    #[test]
    fn validate_pinecone_config_rejects_zero_dimensions() {
        let config = PineconeConfig {
            api_key: Some("key".into()),
            dimensions: 0,
            ..PineconeConfig::default()
        };
        let err = validate_pinecone_config(&config).unwrap_err();
        matches::assert_matches!(err, AppError::Configuration(msg) if msg.contains("dimensions"));
    }

    #[tokio::test]
    async fn new_returns_configuration_error_when_not_implemented() {
        let err = PineconeStore::new("test-key", DEFAULT_PINECONE_ENVIRONMENT)
            .await
            .unwrap_err();
        matches::assert_matches!(err, AppError::Configuration(msg) if {
            msg.contains("not yet implemented") && msg.contains("ares-vector")
        });
    }

    #[tokio::test]
    async fn new_rejects_missing_api_key_before_not_implemented() {
        let err = PineconeStore::new("   ", DEFAULT_PINECONE_ENVIRONMENT)
            .await
            .unwrap_err();
        matches::assert_matches!(err, AppError::Configuration(msg) if msg.contains("api key"));
    }

    #[tokio::test]
    async fn new_error_mentions_alternatives() {
        let err = PineconeStore::new("test-key", DEFAULT_PINECONE_ENVIRONMENT)
            .await
            .unwrap_err();
        let msg = match err {
            AppError::Configuration(m) => m,
            other => panic!("expected Configuration, got {other:?}"),
        };
        assert!(msg.contains("qdrant"), "should suggest qdrant: {msg}");
    }

    #[tokio::test]
    async fn from_config_validates_then_returns_not_implemented() {
        let config = PineconeConfig {
            api_key: Some("test-key".into()),
            ..PineconeConfig::default()
        };
        let err = PineconeStore::from_config(&config).await.unwrap_err();
        matches::assert_matches!(err, AppError::Configuration(_));
    }
}
