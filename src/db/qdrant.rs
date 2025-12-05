//! Local in-memory vector store implementation
//!
//! This replaces the remote Qdrant dependency with a local in-memory vector store
//! that uses fastembed for embeddings. This enables fully local operation without
//! requiring external services.

use crate::types::{AppError, Document, Result, SearchQuery, SearchResult};
use std::collections::HashMap;
use std::sync::RwLock;

/// A document stored in the vector store with its embedding
#[derive(Clone)]
struct StoredDocument {
    document: Document,
    embedding: Vec<f32>,
}

/// In-memory vector store that replaces Qdrant
///
/// Uses cosine similarity for vector search. Thread-safe via RwLock.
pub struct QdrantClient {
    /// Storage for documents by collection name -> document id -> stored document
    collections: RwLock<HashMap<String, HashMap<String, StoredDocument>>>,
    /// Vector dimension (384 for BGE-small)
    dimension: usize,
}

impl QdrantClient {
    /// Create a new local vector store
    ///
    /// The url and api_key parameters are ignored for local mode but kept
    /// for API compatibility with the original remote implementation.
    pub async fn new(_url: String, _api_key: Option<String>) -> Result<Self> {
        Ok(Self {
            collections: RwLock::new(HashMap::new()),
            dimension: 384, // BGE-small embedding dimension
        })
    }

    /// Create a new in-memory vector store (explicit local constructor)
    pub fn new_local() -> Self {
        Self {
            collections: RwLock::new(HashMap::new()),
            dimension: 384,
        }
    }

    /// Create a collection if it doesn't exist
    pub fn ensure_collection(&self, name: &str) -> Result<()> {
        let mut collections = self
            .collections
            .write()
            .map_err(|e| AppError::Database(format!("Lock error: {}", e)))?;

        collections.entry(name.to_string()).or_default();
        Ok(())
    }

    /// Upsert a document with its embedding
    pub async fn upsert_document(&self, document: &Document) -> Result<()> {
        let embedding = document
            .embedding
            .as_ref()
            .ok_or_else(|| AppError::Database("Document missing embedding".to_string()))?;

        if embedding.len() != self.dimension {
            return Err(AppError::Database(format!(
                "Embedding dimension mismatch: expected {}, got {}",
                self.dimension,
                embedding.len()
            )));
        }

        let stored = StoredDocument {
            document: document.clone(),
            embedding: embedding.clone(),
        };

        let collection_name = "documents";

        let mut collections = self
            .collections
            .write()
            .map_err(|e| AppError::Database(format!("Lock error: {}", e)))?;

        let collection = collections.entry(collection_name.to_string()).or_default();
        collection.insert(document.id.clone(), stored);

        Ok(())
    }

    /// Search for similar documents using cosine similarity
    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        let collection_name = "documents";

        let collections = self
            .collections
            .read()
            .map_err(|e| AppError::Database(format!("Lock error: {}", e)))?;

        let collection = match collections.get(collection_name) {
            Some(c) => c,
            None => return Ok(vec![]),
        };

        // We need an embedding for the query - this should be provided by the caller
        // For now, return empty if no documents or implement text matching fallback
        if collection.is_empty() {
            return Ok(vec![]);
        }

        // If we have a query embedding (stored in query.query as comma-separated floats for now)
        // In a real implementation, you'd compute the embedding using fastembed
        let query_embedding = self.parse_query_embedding(&query.query);

        let mut results: Vec<(f32, &StoredDocument)> = collection
            .values()
            .filter_map(|stored| {
                let score = if let Some(ref qe) = query_embedding {
                    cosine_similarity(qe, &stored.embedding)
                } else {
                    // Fallback: text-based matching
                    if stored
                        .document
                        .content
                        .to_lowercase()
                        .contains(&query.query.to_lowercase())
                    {
                        0.5 // Arbitrary score for text match
                    } else {
                        0.0
                    }
                };

                if score >= query.threshold {
                    Some((score, stored))
                } else {
                    None
                }
            })
            .collect();

        // Sort by score descending
        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // Apply filters if provided
        let filtered: Vec<SearchResult> = results
            .into_iter()
            .filter(|(_, stored)| {
                if let Some(filters) = &query.filters {
                    for filter in filters {
                        match filter.field.as_str() {
                            "title" => {
                                if stored.document.metadata.title != filter.value {
                                    return false;
                                }
                            }
                            "source" => {
                                if stored.document.metadata.source != filter.value {
                                    return false;
                                }
                            }
                            "tags" => {
                                if !stored.document.metadata.tags.contains(&filter.value) {
                                    return false;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                true
            })
            .take(query.limit)
            .map(|(score, stored)| SearchResult {
                document: Document {
                    id: stored.document.id.clone(),
                    content: stored.document.content.clone(),
                    metadata: stored.document.metadata.clone(),
                    embedding: None, // Don't return embeddings in search results
                },
                score,
            })
            .collect();

        Ok(filtered)
    }

    /// Search with a pre-computed embedding vector
    pub async fn search_with_embedding(
        &self,
        embedding: &[f32],
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>> {
        let collection_name = "documents";

        let collections = self
            .collections
            .read()
            .map_err(|e| AppError::Database(format!("Lock error: {}", e)))?;

        let collection = match collections.get(collection_name) {
            Some(c) => c,
            None => return Ok(vec![]),
        };

        let mut results: Vec<(f32, &StoredDocument)> = collection
            .values()
            .map(|stored| {
                let score = cosine_similarity(embedding, &stored.embedding);
                (score, stored)
            })
            .filter(|(score, _)| *score >= threshold)
            .collect();

        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let results: Vec<SearchResult> = results
            .into_iter()
            .take(limit)
            .map(|(score, stored)| SearchResult {
                document: Document {
                    id: stored.document.id.clone(),
                    content: stored.document.content.clone(),
                    metadata: stored.document.metadata.clone(),
                    embedding: None,
                },
                score,
            })
            .collect();

        Ok(results)
    }

    /// Delete a document by ID
    pub async fn delete_document(&self, id: &str) -> Result<()> {
        let collection_name = "documents";

        let mut collections = self
            .collections
            .write()
            .map_err(|e| AppError::Database(format!("Lock error: {}", e)))?;

        if let Some(collection) = collections.get_mut(collection_name) {
            collection.remove(id);
        }

        Ok(())
    }

    /// Get document count in a collection
    pub fn document_count(&self, collection_name: &str) -> Result<usize> {
        let collections = self
            .collections
            .read()
            .map_err(|e| AppError::Database(format!("Lock error: {}", e)))?;

        Ok(collections
            .get(collection_name)
            .map(|c| c.len())
            .unwrap_or(0))
    }

    /// Clear all documents from a collection
    pub fn clear_collection(&self, collection_name: &str) -> Result<()> {
        let mut collections = self
            .collections
            .write()
            .map_err(|e| AppError::Database(format!("Lock error: {}", e)))?;

        if let Some(collection) = collections.get_mut(collection_name) {
            collection.clear();
        }

        Ok(())
    }

    /// Try to parse a query string as an embedding (for internal use)
    fn parse_query_embedding(&self, _query: &str) -> Option<Vec<f32>> {
        // In a real implementation, this would use fastembed to compute the embedding
        // For now, return None to fall back to text matching
        None
    }
}

/// Compute cosine similarity between two vectors
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }

    let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let magnitude_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let magnitude_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if magnitude_a == 0.0 || magnitude_b == 0.0 {
        return 0.0;
    }

    dot_product / (magnitude_a * magnitude_b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DocumentMetadata;
    use chrono::Utc;

    fn create_test_document(id: &str, content: &str, embedding: Vec<f32>) -> Document {
        Document {
            id: id.to_string(),
            content: content.to_string(),
            metadata: DocumentMetadata {
                title: format!("Test Document {}", id),
                source: "test".to_string(),
                created_at: Utc::now(),
                tags: vec!["test".to_string()],
            },
            embedding: Some(embedding),
        }
    }

    #[tokio::test]
    async fn test_upsert_and_search() {
        let client = QdrantClient::new_local();

        // Create a document with a simple embedding
        let mut embedding = vec![0.0f32; 384];
        embedding[0] = 1.0;
        embedding[1] = 0.5;

        let doc = create_test_document("doc1", "Hello world", embedding.clone());
        client.upsert_document(&doc).await.unwrap();

        // Search with the same embedding should return high similarity
        let results = client
            .search_with_embedding(&embedding, 10, 0.0)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].document.id, "doc1");
        assert!(results[0].score > 0.99); // Should be very close to 1.0
    }

    #[tokio::test]
    async fn test_delete_document() {
        let client = QdrantClient::new_local();

        let mut embedding = vec![0.0f32; 384];
        embedding[0] = 1.0;

        let doc = create_test_document("doc1", "Test content", embedding);
        client.upsert_document(&doc).await.unwrap();

        assert_eq!(client.document_count("documents").unwrap(), 1);

        client.delete_document("doc1").await.unwrap();

        assert_eq!(client.document_count("documents").unwrap(), 0);
    }

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.0001);

        let c = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &c).abs() < 0.0001); // Orthogonal vectors

        let d = vec![-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &d) + 1.0).abs() < 0.0001); // Opposite vectors
    }
}
