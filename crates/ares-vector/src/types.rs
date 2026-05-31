//! Common types for ares-vector.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unique identifier for a vector in a collection.
pub type VectorId = String;

/// Metadata associated with a vector.
///
/// Arbitrary key-value pairs that can be stored alongside vectors.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VectorMetadata {
    /// Key-value pairs of metadata.
    pub data: HashMap<String, MetadataValue>,
}

impl VectorMetadata {
    /// Create empty metadata.
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    /// Create metadata from a list of key-value pairs.
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<MetadataValue>,
    {
        Self {
            data: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    /// Insert a key-value pair.
    pub fn insert<K: Into<String>, V: Into<MetadataValue>>(&mut self, key: K, value: V) {
        self.data.insert(key.into(), value.into());
    }

    /// Get a value by key.
    pub fn get(&self, key: &str) -> Option<&MetadataValue> {
        self.data.get(key)
    }

    /// Get a string value by key.
    pub fn get_string(&self, key: &str) -> Option<&str> {
        match self.data.get(key)? {
            MetadataValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// Get an integer value by key.
    pub fn get_int(&self, key: &str) -> Option<i64> {
        match self.data.get(key)? {
            MetadataValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Get a float value by key.
    pub fn get_float(&self, key: &str) -> Option<f64> {
        match self.data.get(key)? {
            MetadataValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Get a boolean value by key.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.data.get(key)? {
            MetadataValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Check if metadata is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Get the number of metadata entries.
    pub fn len(&self) -> usize {
        self.data.len()
    }
}

/// A metadata value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MetadataValue {
    /// String value.
    String(String),
    /// Integer value.
    Int(i64),
    /// Float value.
    Float(f64),
    /// Boolean value.
    Bool(bool),
    /// List of values.
    List(Vec<MetadataValue>),
}

impl From<String> for MetadataValue {
    fn from(s: String) -> Self {
        MetadataValue::String(s)
    }
}

impl From<&str> for MetadataValue {
    fn from(s: &str) -> Self {
        MetadataValue::String(s.to_string())
    }
}

impl From<i64> for MetadataValue {
    fn from(i: i64) -> Self {
        MetadataValue::Int(i)
    }
}

impl From<i32> for MetadataValue {
    fn from(i: i32) -> Self {
        MetadataValue::Int(i as i64)
    }
}

impl From<f64> for MetadataValue {
    fn from(f: f64) -> Self {
        MetadataValue::Float(f)
    }
}

impl From<f32> for MetadataValue {
    fn from(f: f32) -> Self {
        MetadataValue::Float(f as f64)
    }
}

impl From<bool> for MetadataValue {
    fn from(b: bool) -> Self {
        MetadataValue::Bool(b)
    }
}

impl<T: Into<MetadataValue>> From<Vec<T>> for MetadataValue {
    fn from(v: Vec<T>) -> Self {
        MetadataValue::List(v.into_iter().map(Into::into).collect())
    }
}

/// Result of a vector search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// ID of the matched vector.
    pub id: VectorId,
    /// Similarity score (higher = more similar for most metrics).
    pub score: f32,
    /// Optional metadata associated with the vector.
    pub metadata: Option<VectorMetadata>,
}

impl SearchResult {
    /// Create a new search result.
    pub fn new(id: VectorId, score: f32, metadata: Option<VectorMetadata>) -> Self {
        Self {
            id,
            score,
            metadata,
        }
    }
}

/// Internal representation of a stored vector.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredVector {
    /// External string ID.
    pub id: VectorId,
    /// Internal numeric ID for HNSW.
    pub internal_id: usize,
    /// The vector data.
    pub vector: Vec<f32>,
    /// Optional metadata.
    pub metadata: Option<VectorMetadata>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_basic() {
        let mut meta = VectorMetadata::new();
        meta.insert("title", "Test Document");
        meta.insert("score", 0.95f64);
        meta.insert("count", 42i64);
        meta.insert("active", true);

        assert_eq!(meta.get_string("title"), Some("Test Document"));
        assert_eq!(meta.get_float("score"), Some(0.95));
        assert_eq!(meta.get_int("count"), Some(42));
        assert_eq!(meta.get_bool("active"), Some(true));
    }

    #[test]
    fn test_metadata_from_pairs() {
        let meta = VectorMetadata::from_pairs([
            ("key1", MetadataValue::String("value1".to_string())),
            ("key2", MetadataValue::Int(123)),
        ]);

        assert_eq!(meta.len(), 2);
        assert_eq!(meta.get_string("key1"), Some("value1"));
        assert_eq!(meta.get_int("key2"), Some(123));
    }

    #[test]
    fn test_search_result() {
        let result = SearchResult::new(
            "doc1".to_string(),
            0.95,
            Some(VectorMetadata::from_pairs([(
                "title",
                MetadataValue::String("Test".to_string()),
            )])),
        );

        assert_eq!(result.id, "doc1");
        assert_eq!(result.score, 0.95);
        assert!(result.metadata.is_some());

        let bare = SearchResult::new("doc2".to_string(), 0.5, None);
        assert!(bare.metadata.is_none());
    }

    #[test]
    fn test_metadata_get_and_empty() {
        let empty = VectorMetadata::new();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert!(empty.get("missing").is_none());

        let mut meta = VectorMetadata::new();
        meta.insert("tag", "alpha");
        assert!(!meta.is_empty());
        assert_eq!(meta.len(), 1);
        assert_eq!(
            meta.get("tag"),
            Some(&MetadataValue::String("alpha".to_string()))
        );
        assert!(meta.get("missing").is_none());
    }

    #[test]
    fn test_metadata_typed_getters_wrong_type() {
        let mut meta = VectorMetadata::new();
        meta.insert("name", "hello");
        meta.insert("count", 7i64);
        meta.insert("ratio", 0.5f64);
        meta.insert("active", false);

        assert_eq!(meta.get_string("name"), Some("hello"));
        assert_eq!(meta.get_string("count"), None);
        assert_eq!(meta.get_int("count"), Some(7));
        assert_eq!(meta.get_int("name"), None);
        assert_eq!(meta.get_float("ratio"), Some(0.5));
        assert_eq!(meta.get_float("count"), None);
        assert_eq!(meta.get_bool("active"), Some(false));
        assert_eq!(meta.get_bool("ratio"), None);
    }

    #[test]
    fn test_metadata_value_from_impls() {
        let from_string: MetadataValue = MetadataValue::from(String::from("text"));
        let from_str: MetadataValue = "text".into();
        let from_i32: MetadataValue = 42i32.into();
        let from_i64: MetadataValue = 99i64.into();
        let from_f32: MetadataValue = 1.5f32.into();
        let from_f64: MetadataValue = 2.5f64.into();
        let from_bool: MetadataValue = true.into();
        let from_list: MetadataValue = vec!["a", "b"].into();

        assert_eq!(from_string, MetadataValue::String("text".to_string()));
        assert_eq!(from_str, MetadataValue::String("text".to_string()));
        assert_eq!(from_i32, MetadataValue::Int(42));
        assert_eq!(from_i64, MetadataValue::Int(99));
        assert_eq!(from_f32, MetadataValue::Float(1.5));
        assert_eq!(from_f64, MetadataValue::Float(2.5));
        assert_eq!(from_bool, MetadataValue::Bool(true));
        assert_eq!(
            from_list,
            MetadataValue::List(vec![
                MetadataValue::String("a".to_string()),
                MetadataValue::String("b".to_string()),
            ])
        );
    }

    #[test]
    fn test_stored_vector_roundtrip() {
        let stored = StoredVector {
            id: "vec-1".to_string(),
            internal_id: 0,
            vector: vec![1.0, 2.0, 3.0],
            metadata: Some(VectorMetadata::from_pairs([("k", "v")])),
        };
        let json = serde_json::to_string(&stored).unwrap();
        let decoded: StoredVector = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.id, "vec-1");
        assert_eq!(decoded.internal_id, 0);
        assert_eq!(decoded.vector, vec![1.0, 2.0, 3.0]);
        assert_eq!(decoded.metadata.as_ref().unwrap().get_string("k"), Some("v"));
    }
}
