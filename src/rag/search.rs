//! Search strategies for RAG pipeline.
//!
//! This module implements multiple search strategies that can be combined
//! for optimal retrieval:
//! - **Semantic search**: Dense vector similarity using embeddings
//! - **BM25 search**: Sparse lexical matching (TF-IDF variant)
//! - **Fuzzy search**: Approximate string matching for typo tolerance
//! - **Hybrid search**: Combines multiple strategies with RRF fusion
//!
//! # Persistence
//!
//! The BM25 and fuzzy indices support persistence via `save()` and `load()` methods.
//! This allows the indices to survive server restarts without re-indexing.
//!
//! ```ignore
//! // Save index to disk
//! bm25_index.save("data/bm25_index.json")?;
//!
//! // Load index from disk
//! let bm25_index = Bm25Index::load("data/bm25_index.json")?;
//! ```

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::types::{AppError, Document, Result};

// ============================================================================
// Search Strategy Types
// ============================================================================

/// Available search strategies
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SearchStrategy {
    /// Semantic similarity using dense embeddings
    #[default]
    Semantic,
    /// BM25 lexical search (sparse)
    Bm25,
    /// Fuzzy string matching
    Fuzzy,
    /// Hybrid combining multiple strategies
    Hybrid,
}

impl FromStr for SearchStrategy {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "semantic" | "dense" | "vector" => Ok(Self::Semantic),
            "bm25" | "lexical" | "sparse" => Ok(Self::Bm25),
            "fuzzy" | "approximate" => Ok(Self::Fuzzy),
            "hybrid" | "combined" | "rrf" => Ok(Self::Hybrid),
            _ => Err(AppError::Internal(format!(
                "Unknown search strategy: {}. Use: semantic, bm25, fuzzy, hybrid",
                s
            ))),
        }
    }
}

impl std::fmt::Display for SearchStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Semantic => "semantic",
            Self::Bm25 => "bm25",
            Self::Fuzzy => "fuzzy",
            Self::Hybrid => "hybrid",
        };
        write!(f, "{}", name)
    }
}

// ============================================================================
// Search Result Types
// ============================================================================

/// A single search result with score and optional metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Document ID
    pub id: String,
    /// Document content
    pub content: String,
    /// Relevance score (higher is better)
    pub score: f32,
    /// Which strategies contributed to this result
    pub sources: Vec<SearchStrategy>,
    /// Original document metadata
    pub metadata: Option<serde_json::Value>,
}

/// A correction made to a query word during typo correction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryCorrection {
    /// Original (misspelled) word
    pub original: String,
    /// Corrected word from vocabulary
    pub corrected: String,
    /// Edit distance between original and corrected
    pub distance: usize,
}

/// Search request configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    /// Query text
    pub query: String,
    /// Search strategy to use
    #[serde(default)]
    pub strategy: SearchStrategy,
    /// Maximum number of results
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    /// Minimum score threshold
    #[serde(default)]
    pub min_score: f32,
    /// Enable reranking
    #[serde(default)]
    pub rerank: bool,
    /// Collection to search in
    pub collection: String,
    /// Weights for hybrid search components
    #[serde(default)]
    pub hybrid_weights: HybridWeights,
}

fn default_top_k() -> usize {
    10
}

/// Weights for hybrid search strategy components
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HybridWeights {
    /// Weight for semantic search (0.0 - 1.0)
    pub semantic: f32,
    /// Weight for BM25 search (0.0 - 1.0)
    pub bm25: f32,
    /// Weight for fuzzy search (0.0 - 1.0)
    pub fuzzy: f32,
}

impl Default for HybridWeights {
    fn default() -> Self {
        Self {
            semantic: 0.6,
            bm25: 0.3,
            fuzzy: 0.1,
        }
    }
}

// ============================================================================
// BM25 Implementation
// ============================================================================

/// BM25 search index for lexical matching
///
/// This index supports persistence via `save()` and `load()` methods.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bm25Index {
    /// Document ID -> tokenized content
    documents: HashMap<String, Vec<String>>,
    /// Term -> document IDs containing term
    inverted_index: HashMap<String, HashSet<String>>,
    /// Document frequencies for each term
    document_frequencies: HashMap<String, usize>,
    /// Total number of documents
    doc_count: usize,
    /// Average document length
    avg_doc_length: f32,
    /// BM25 k1 parameter (term frequency saturation)
    k1: f32,
    /// BM25 b parameter (length normalization)
    b: f32,
}

impl Bm25Index {
    /// Create a new BM25 index with default parameters
    pub fn new() -> Self {
        Self {
            k1: 1.2,
            b: 0.75,
            ..Default::default()
        }
    }

    /// Create with custom BM25 parameters
    pub fn with_params(k1: f32, b: f32) -> Self {
        Self {
            k1,
            b,
            ..Default::default()
        }
    }

    /// Tokenize text into lowercase terms
    fn tokenize(text: &str) -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty() && s.len() > 1)
            .map(String::from)
            .collect()
    }

    /// Add a document to the index
    pub fn add_document(&mut self, id: &str, content: &str) {
        let tokens = Self::tokenize(content);

        // Update document frequency for each unique term
        let unique_terms: HashSet<_> = tokens.iter().cloned().collect();
        for term in &unique_terms {
            *self.document_frequencies.entry(term.clone()).or_insert(0) += 1;
            self.inverted_index
                .entry(term.clone())
                .or_default()
                .insert(id.to_string());
        }

        // Store tokenized document
        self.documents.insert(id.to_string(), tokens);
        self.doc_count += 1;

        // Update average document length
        let total_tokens: usize = self.documents.values().map(|v| v.len()).sum();
        self.avg_doc_length = total_tokens as f32 / self.doc_count as f32;
    }

    /// Remove a document from the index
    pub fn remove_document(&mut self, id: &str) {
        if let Some(tokens) = self.documents.remove(id) {
            let unique_terms: HashSet<_> = tokens.into_iter().collect();
            for term in unique_terms {
                if let Some(df) = self.document_frequencies.get_mut(&term) {
                    *df = df.saturating_sub(1);
                    if *df == 0 {
                        self.document_frequencies.remove(&term);
                    }
                }
                if let Some(docs) = self.inverted_index.get_mut(&term) {
                    docs.remove(id);
                    if docs.is_empty() {
                        self.inverted_index.remove(&term);
                    }
                }
            }
            self.doc_count = self.doc_count.saturating_sub(1);

            // Recalculate average
            if self.doc_count > 0 {
                let total_tokens: usize = self.documents.values().map(|v| v.len()).sum();
                self.avg_doc_length = total_tokens as f32 / self.doc_count as f32;
            } else {
                self.avg_doc_length = 0.0;
            }
        }
    }

    /// Calculate IDF (Inverse Document Frequency) for a term
    fn idf(&self, term: &str) -> f32 {
        let df = self.document_frequencies.get(term).copied().unwrap_or(0) as f32;
        let n = self.doc_count as f32;
        if df == 0.0 || n == 0.0 {
            return 0.0;
        }
        ((n - df + 0.5) / (df + 0.5) + 1.0).ln()
    }

    /// Calculate BM25 score for a document given a query
    fn score_document(&self, doc_id: &str, query_terms: &[String]) -> f32 {
        let doc_tokens = match self.documents.get(doc_id) {
            Some(tokens) => tokens,
            None => return 0.0,
        };

        let doc_len = doc_tokens.len() as f32;
        let mut score = 0.0;

        // Count term frequencies in document
        let mut term_freq: HashMap<&str, usize> = HashMap::new();
        for token in doc_tokens {
            *term_freq.entry(token.as_str()).or_insert(0) += 1;
        }

        for term in query_terms {
            let tf = term_freq.get(term.as_str()).copied().unwrap_or(0) as f32;
            let idf = self.idf(term);

            // BM25 formula
            let numerator = tf * (self.k1 + 1.0);
            let denominator =
                tf + self.k1 * (1.0 - self.b + self.b * doc_len / self.avg_doc_length);
            score += idf * numerator / denominator;
        }

        score
    }

    /// Search the index and return top-k results
    pub fn search(&self, query: &str, top_k: usize) -> Vec<(String, f32)> {
        let query_terms = Self::tokenize(query);
        if query_terms.is_empty() {
            return Vec::new();
        }

        // Find candidate documents (those containing at least one query term)
        let mut candidates: HashSet<String> = HashSet::new();
        for term in &query_terms {
            if let Some(docs) = self.inverted_index.get(term) {
                candidates.extend(docs.iter().cloned());
            }
        }

        // Score all candidates
        let mut results: Vec<(String, f32)> = candidates
            .iter()
            .map(|id| {
                let score = self.score_document(id, &query_terms);
                (id.clone(), score)
            })
            .filter(|(_, score)| *score > 0.0)
            .collect();

        // Sort by score descending
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Return top-k
        results.truncate(top_k);
        results
    }

    /// Get the number of documents in the index
    pub fn len(&self) -> usize {
        self.doc_count
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.doc_count == 0
    }

    /// Clear the index
    pub fn clear(&mut self) {
        self.documents.clear();
        self.inverted_index.clear();
        self.document_frequencies.clear();
        self.doc_count = 0;
        self.avg_doc_length = 0.0;
    }

    /// Save the index to a file (JSON format)
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written or serialization fails.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let json = serde_json::to_string(self)
            .map_err(|e| AppError::Internal(format!("Failed to serialize BM25 index: {}", e)))?;
        std::fs::write(path, json)
            .map_err(|e| AppError::Internal(format!("Failed to write BM25 index file: {}", e)))?;
        Ok(())
    }

    /// Load the index from a file (JSON format)
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or deserialization fails.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| AppError::Internal(format!("Failed to read BM25 index file: {}", e)))?;
        let index: Self = serde_json::from_str(&json)
            .map_err(|e| AppError::Internal(format!("Failed to deserialize BM25 index: {}", e)))?;
        Ok(index)
    }

    /// Load the index from a file if it exists, otherwise return a new empty index
    pub fn load_or_new<P: AsRef<Path>>(path: P) -> Self {
        if path.as_ref().exists() {
            Self::load(path).unwrap_or_else(|_| Self::new())
        } else {
            Self::new()
        }
    }
}

// ============================================================================
// Fuzzy Search Implementation
// ============================================================================

/// Fuzzy search using Levenshtein distance
///
/// This index supports persistence via `save()` and `load()` methods.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FuzzyIndex {
    /// Document ID -> content for fuzzy matching
    documents: HashMap<String, String>,
    /// Vocabulary of all unique words in the index (for query correction)
    vocabulary: HashSet<String>,
    /// Maximum edit distance for fuzzy matches
    max_distance: usize,
}

impl FuzzyIndex {
    /// Create a new fuzzy index
    pub fn new() -> Self {
        Self {
            max_distance: 2,
            ..Default::default()
        }
    }

    /// Create with custom max edit distance
    pub fn with_max_distance(max_distance: usize) -> Self {
        Self {
            max_distance,
            ..Default::default()
        }
    }

    /// Tokenize text into lowercase words
    fn tokenize(text: &str) -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty() && s.len() > 1)
            .map(String::from)
            .collect()
    }

    /// Add a document to the index
    pub fn add_document(&mut self, id: &str, content: &str) {
        let lower_content = content.to_lowercase();

        // Add words to vocabulary
        for word in Self::tokenize(&lower_content) {
            self.vocabulary.insert(word);
        }

        self.documents.insert(id.to_string(), lower_content);
    }

    /// Remove a document from the index
    pub fn remove_document(&mut self, id: &str) {
        self.documents.remove(id);
        // Note: We don't remove from vocabulary as words may be in other docs
        // Vocabulary is rebuilt on clear()
    }

    /// Calculate Levenshtein distance between two strings
    fn levenshtein_distance(s1: &str, s2: &str) -> usize {
        let len1 = s1.chars().count();
        let len2 = s2.chars().count();

        if len1 == 0 {
            return len2;
        }
        if len2 == 0 {
            return len1;
        }

        let s1_chars: Vec<char> = s1.chars().collect();
        let s2_chars: Vec<char> = s2.chars().collect();

        let mut prev_row: Vec<usize> = (0..=len2).collect();
        let mut curr_row = vec![0; len2 + 1];

        for (i, c1) in s1_chars.iter().enumerate() {
            curr_row[0] = i + 1;

            for (j, c2) in s2_chars.iter().enumerate() {
                let cost = if c1 == c2 { 0 } else { 1 };
                curr_row[j + 1] = (prev_row[j + 1] + 1)
                    .min(curr_row[j] + 1)
                    .min(prev_row[j] + cost);
            }

            std::mem::swap(&mut prev_row, &mut curr_row);
        }

        prev_row[len2]
    }

    /// Find the best matching word from vocabulary for a given query word
    /// Returns the corrected word and the edit distance
    pub fn correct_word(&self, word: &str) -> Option<(String, usize)> {
        let word_lower = word.to_lowercase();

        // If exact match exists, return it
        if self.vocabulary.contains(&word_lower) {
            return Some((word_lower, 0));
        }

        // Find best fuzzy match within max_distance
        let mut best_match: Option<(String, usize)> = None;

        for vocab_word in &self.vocabulary {
            // Skip if length difference is too large (can't be within max_distance)
            let len_diff = (word_lower.len() as isize - vocab_word.len() as isize).unsigned_abs();
            if len_diff > self.max_distance {
                continue;
            }

            let distance = Self::levenshtein_distance(&word_lower, vocab_word);
            if distance <= self.max_distance {
                match &best_match {
                    None => best_match = Some((vocab_word.clone(), distance)),
                    Some((_, best_dist)) if distance < *best_dist => {
                        best_match = Some((vocab_word.clone(), distance));
                    }
                    _ => {}
                }
            }
        }

        best_match
    }

    /// Correct a query by replacing typos with vocabulary words
    /// Returns the corrected query and a list of corrections made
    pub fn correct_query(&self, query: &str) -> (String, Vec<QueryCorrection>) {
        let words = Self::tokenize(query);
        let mut corrected_words = Vec::with_capacity(words.len());
        let mut corrections = Vec::new();

        for word in &words {
            if let Some((corrected, distance)) = self.correct_word(word) {
                if distance > 0 {
                    corrections.push(QueryCorrection {
                        original: word.clone(),
                        corrected: corrected.clone(),
                        distance,
                    });
                }
                corrected_words.push(corrected);
            } else {
                // No match found, keep original
                corrected_words.push(word.clone());
            }
        }

        (corrected_words.join(" "), corrections)
    }

    /// Calculate fuzzy match score (1.0 - normalized distance)
    fn fuzzy_score(query: &str, text: &str, max_distance: usize) -> f32 {
        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        // Try to find each query word in the text with fuzzy matching
        let mut total_score = 0.0;
        let mut matched_words = 0;

        for query_word in &query_words {
            let mut best_score = 0.0f32;

            for text_word in text.split_whitespace() {
                if text_word.len() < 2 {
                    continue;
                }

                let distance = Self::levenshtein_distance(query_word, text_word);
                if distance <= max_distance {
                    let max_len = query_word.len().max(text_word.len());
                    let score = 1.0 - (distance as f32 / max_len as f32);
                    best_score = best_score.max(score);
                }
            }

            if best_score > 0.0 {
                total_score += best_score;
                matched_words += 1;
            }
        }

        if matched_words > 0 {
            (total_score / query_words.len() as f32)
                * (matched_words as f32 / query_words.len() as f32)
        } else {
            0.0
        }
    }

    /// Search the index with fuzzy matching
    pub fn search(&self, query: &str, top_k: usize) -> Vec<(String, f32)> {
        let mut results: Vec<(String, f32)> = self
            .documents
            .iter()
            .filter_map(|(id, content)| {
                let score = Self::fuzzy_score(query, content, self.max_distance);
                if score > 0.0 {
                    Some((id.clone(), score))
                } else {
                    None
                }
            })
            .collect();

        // Sort by score descending
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Return top-k
        results.truncate(top_k);
        results
    }

    /// Get the number of documents in the index
    pub fn len(&self) -> usize {
        self.documents.len()
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }

    /// Clear the index
    pub fn clear(&mut self) {
        self.documents.clear();
        self.vocabulary.clear();
    }

    /// Get the vocabulary size (number of unique words)
    pub fn vocabulary_size(&self) -> usize {
        self.vocabulary.len()
    }

    /// Save the index to a file (JSON format)
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written or serialization fails.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let json = serde_json::to_string(self)
            .map_err(|e| AppError::Internal(format!("Failed to serialize fuzzy index: {}", e)))?;
        std::fs::write(path, json)
            .map_err(|e| AppError::Internal(format!("Failed to write fuzzy index file: {}", e)))?;
        Ok(())
    }

    /// Load the index from a file (JSON format)
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or deserialization fails.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| AppError::Internal(format!("Failed to read fuzzy index file: {}", e)))?;
        let index: Self = serde_json::from_str(&json)
            .map_err(|e| AppError::Internal(format!("Failed to deserialize fuzzy index: {}", e)))?;
        Ok(index)
    }

    /// Load the index from a file if it exists, otherwise return a new empty index
    pub fn load_or_new<P: AsRef<Path>>(path: P) -> Self {
        if path.as_ref().exists() {
            Self::load(path).unwrap_or_else(|_| Self::new())
        } else {
            Self::new()
        }
    }
}

// ============================================================================
// Reciprocal Rank Fusion (RRF)
// ============================================================================

/// Reciprocal Rank Fusion for combining multiple ranked lists
#[derive(Debug, Clone)]
pub struct RrfFusion {
    /// RRF constant (typically 60)
    k: f32,
}

impl Default for RrfFusion {
    fn default() -> Self {
        Self { k: 60.0 }
    }
}

impl RrfFusion {
    /// Create a new RRF fusion with default k=60
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with custom k parameter
    pub fn with_k(k: f32) -> Self {
        Self { k }
    }

    /// Fuse multiple ranked result lists with weights
    ///
    /// Each input is a tuple of (results, weight) where results is (doc_id, score)
    pub fn fuse(&self, ranked_lists: &[(&[(String, f32)], f32)]) -> Vec<(String, f32)> {
        let mut fused_scores: HashMap<String, f32> = HashMap::new();

        for (results, weight) in ranked_lists {
            for (rank, (doc_id, _score)) in results.iter().enumerate() {
                // RRF formula: 1 / (k + rank)
                let rrf_score = weight / (self.k + rank as f32 + 1.0);
                *fused_scores.entry(doc_id.clone()).or_insert(0.0) += rrf_score;
            }
        }

        // Convert to sorted vector
        let mut results: Vec<_> = fused_scores.into_iter().collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }
}

// ============================================================================
// Unified Search Engine
// ============================================================================

/// Unified search engine combining multiple strategies
///
/// This engine supports persistence via `save()` and `load()` methods.
/// The engine stores both BM25 and fuzzy indices in separate files within
/// a directory.
#[derive(Debug, Default)]
pub struct SearchEngine {
    /// BM25 lexical index
    pub bm25: Bm25Index,
    /// Fuzzy search index
    pub fuzzy: FuzzyIndex,
    /// RRF fusion for hybrid search
    pub rrf: RrfFusion,
}

impl SearchEngine {
    /// Create a new search engine
    pub fn new() -> Self {
        Self::default()
    }

    /// Index a document for all search strategies
    pub fn index_document(&mut self, doc: &Document) {
        self.bm25.add_document(&doc.id, &doc.content);
        self.fuzzy.add_document(&doc.id, &doc.content);
    }

    /// Index multiple documents
    pub fn index_documents(&mut self, docs: &[Document]) {
        for doc in docs {
            self.index_document(doc);
        }
    }

    /// Remove a document from all indices
    pub fn remove_document(&mut self, id: &str) {
        self.bm25.remove_document(id);
        self.fuzzy.remove_document(id);
    }

    /// Perform BM25 search
    pub fn search_bm25(&self, query: &str, top_k: usize) -> Vec<(String, f32)> {
        self.bm25.search(query, top_k)
    }

    /// Perform fuzzy search
    pub fn search_fuzzy(&self, query: &str, top_k: usize) -> Vec<(String, f32)> {
        self.fuzzy.search(query, top_k)
    }

    /// Perform hybrid search with configurable weights
    ///
    /// `semantic_results` should be pre-computed from the vector store
    pub fn search_hybrid(
        &self,
        query: &str,
        semantic_results: &[(String, f32)],
        weights: &HybridWeights,
        top_k: usize,
    ) -> Vec<(String, f32)> {
        let bm25_results = self.bm25.search(query, top_k * 2);
        let fuzzy_results = self.fuzzy.search(query, top_k * 2);

        let ranked_lists: Vec<(&[(String, f32)], f32)> = vec![
            (semantic_results, weights.semantic),
            (&bm25_results, weights.bm25),
            (&fuzzy_results, weights.fuzzy),
        ];

        let mut fused = self.rrf.fuse(&ranked_lists);
        fused.truncate(top_k);
        fused
    }

    /// Perform BM25 search with automatic query typo correction
    ///
    /// This corrects misspelled words in the query using the vocabulary
    /// built from indexed documents, then performs BM25 search with the
    /// corrected query.
    ///
    /// Returns a tuple of (results, corrected_query, corrections_made)
    pub fn search_bm25_with_correction(
        &self,
        query: &str,
        top_k: usize,
    ) -> (Vec<(String, f32)>, String, Vec<QueryCorrection>) {
        let (corrected_query, corrections) = self.fuzzy.correct_query(query);
        let results = self.bm25.search(&corrected_query, top_k);
        (results, corrected_query, corrections)
    }

    /// Perform hybrid search with automatic query typo correction
    ///
    /// This corrects misspelled words in the query, then performs hybrid search
    /// combining semantic, BM25, and fuzzy results.
    ///
    /// Note: semantic_results should be computed using the corrected query
    /// for best results.
    ///
    /// Returns a tuple of (results, corrected_query, corrections_made)
    pub fn search_hybrid_with_correction(
        &self,
        query: &str,
        semantic_results: &[(String, f32)],
        weights: &HybridWeights,
        top_k: usize,
    ) -> (Vec<(String, f32)>, String, Vec<QueryCorrection>) {
        let (corrected_query, corrections) = self.fuzzy.correct_query(query);
        let results = self.search_hybrid(&corrected_query, semantic_results, weights, top_k);
        (results, corrected_query, corrections)
    }

    /// Clear all indices
    pub fn clear(&mut self) {
        self.bm25.clear();
        self.fuzzy.clear();
    }

    /// Get the number of indexed documents
    pub fn len(&self) -> usize {
        self.bm25.len()
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.bm25.is_empty()
    }

    /// Save all indices to a directory
    ///
    /// Creates the directory if it doesn't exist. Saves:
    /// - `bm25_index.json` - BM25 lexical index
    /// - `fuzzy_index.json` - Fuzzy search index
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created or files cannot be written.
    pub fn save<P: AsRef<Path>>(&self, dir: P) -> Result<()> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).map_err(|e| {
            AppError::Internal(format!("Failed to create search index directory: {}", e))
        })?;

        self.bm25.save(dir.join("bm25_index.json"))?;
        self.fuzzy.save(dir.join("fuzzy_index.json"))?;

        Ok(())
    }

    /// Load all indices from a directory
    ///
    /// # Errors
    ///
    /// Returns an error if the files cannot be read or deserialization fails.
    pub fn load<P: AsRef<Path>>(dir: P) -> Result<Self> {
        let dir = dir.as_ref();
        let bm25 = Bm25Index::load(dir.join("bm25_index.json"))?;
        let fuzzy = FuzzyIndex::load(dir.join("fuzzy_index.json"))?;

        Ok(Self {
            bm25,
            fuzzy,
            rrf: RrfFusion::default(),
        })
    }

    /// Load indices from a directory if they exist, otherwise return a new empty engine
    pub fn load_or_new<P: AsRef<Path>>(dir: P) -> Self {
        let dir = dir.as_ref();
        if dir.exists() {
            Self::load(dir).unwrap_or_else(|_| Self::new())
        } else {
            Self::new()
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_strategy_from_str() {
        assert_eq!(
            "semantic".parse::<SearchStrategy>().unwrap(),
            SearchStrategy::Semantic
        );
        assert_eq!(
            "bm25".parse::<SearchStrategy>().unwrap(),
            SearchStrategy::Bm25
        );
        assert_eq!(
            "fuzzy".parse::<SearchStrategy>().unwrap(),
            SearchStrategy::Fuzzy
        );
        assert_eq!(
            "hybrid".parse::<SearchStrategy>().unwrap(),
            SearchStrategy::Hybrid
        );
    }

    #[test]
    fn test_bm25_basic() {
        let mut index = Bm25Index::new();
        index.add_document("doc1", "The quick brown fox jumps over the lazy dog");
        index.add_document("doc2", "A fast brown fox leaps over sleeping dogs");
        index.add_document("doc3", "The cat sleeps on the mat");

        let results = index.search("quick brown fox", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, "doc1"); // Best match for "quick brown fox"
    }

    #[test]
    fn test_bm25_ranking() {
        let mut index = Bm25Index::new();
        index.add_document("doc1", "apple apple apple");
        index.add_document("doc2", "apple banana");
        index.add_document("doc3", "banana banana banana");

        let results = index.search("apple", 10);
        assert!(!results.is_empty());
        // doc1 should score higher (more term frequency)
        assert_eq!(results[0].0, "doc1");
    }

    #[test]
    fn test_bm25_remove_document() {
        let mut index = Bm25Index::new();
        index.add_document("doc1", "hello world");
        index.add_document("doc2", "goodbye world");

        assert_eq!(index.len(), 2);

        index.remove_document("doc1");
        assert_eq!(index.len(), 1);

        let results = index.search("hello", 10);
        assert!(results.is_empty()); // doc1 was removed
    }

    #[test]
    fn test_fuzzy_exact_match() {
        let mut index = FuzzyIndex::new();
        index.add_document("doc1", "machine learning algorithms");
        index.add_document("doc2", "deep neural networks");

        let results = index.search("machine", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, "doc1");
    }

    #[test]
    fn test_fuzzy_typo_tolerance() {
        let mut index = FuzzyIndex::with_max_distance(2);
        index.add_document("doc1", "machine learning");
        index.add_document("doc2", "deep learning");

        // "machne" is 1 edit away from "machine"
        let results = index.search("machne", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, "doc1");
    }

    #[test]
    fn test_levenshtein_distance() {
        assert_eq!(FuzzyIndex::levenshtein_distance("kitten", "sitting"), 3);
        assert_eq!(FuzzyIndex::levenshtein_distance("hello", "hello"), 0);
        assert_eq!(FuzzyIndex::levenshtein_distance("", "abc"), 3);
        assert_eq!(FuzzyIndex::levenshtein_distance("abc", ""), 3);
    }

    #[test]
    fn test_rrf_fusion() {
        let rrf = RrfFusion::new();

        let list1 = [
            ("doc1".to_string(), 0.9),
            ("doc2".to_string(), 0.8),
            ("doc3".to_string(), 0.7),
        ];

        let list2 = [
            ("doc2".to_string(), 0.95),
            ("doc1".to_string(), 0.85),
            ("doc4".to_string(), 0.75),
        ];

        let ranked_lists = vec![(&list1[..], 1.0), (&list2[..], 1.0)];
        let fused = rrf.fuse(&ranked_lists);

        // doc1 and doc2 appear in both lists, should be top
        assert!(!fused.is_empty());
        let top_ids: Vec<_> = fused.iter().take(2).map(|(id, _)| id.clone()).collect();
        assert!(top_ids.contains(&"doc1".to_string()));
        assert!(top_ids.contains(&"doc2".to_string()));
    }

    #[test]
    fn test_search_engine_integration() {
        let mut engine = SearchEngine::new();

        let docs = vec![
            Document {
                id: "doc1".to_string(),
                content: "Rust programming language is fast and memory safe".to_string(),
                metadata: Default::default(),
                embedding: None,
            },
            Document {
                id: "doc2".to_string(),
                content: "Python is popular for machine learning and data science".to_string(),
                metadata: Default::default(),
                embedding: None,
            },
            Document {
                id: "doc3".to_string(),
                content: "JavaScript runs in web browsers".to_string(),
                metadata: Default::default(),
                embedding: None,
            },
        ];

        engine.index_documents(&docs);
        assert_eq!(engine.len(), 3);

        // BM25 search
        let bm25_results = engine.search_bm25("Rust programming", 10);
        assert!(!bm25_results.is_empty());
        assert_eq!(bm25_results[0].0, "doc1");

        // Fuzzy search - test with exact word (fuzzy should handle it)
        let fuzzy_results = engine.search_fuzzy("rust", 10);
        // Fuzzy search should find "rust" with exact match
        assert!(!fuzzy_results.is_empty(), "Fuzzy search should find 'rust'");
    }

    #[test]
    fn test_hybrid_search() {
        let mut engine = SearchEngine::new();

        let docs = vec![
            Document {
                id: "doc1".to_string(),
                content: "Vector databases enable semantic search".to_string(),
                metadata: Default::default(),
                embedding: None,
            },
            Document {
                id: "doc2".to_string(),
                content: "BM25 is a lexical search algorithm".to_string(),
                metadata: Default::default(),
                embedding: None,
            },
        ];

        engine.index_documents(&docs);

        // Simulate semantic results
        let semantic_results = vec![("doc1".to_string(), 0.95), ("doc2".to_string(), 0.80)];

        let weights = HybridWeights {
            semantic: 0.5,
            bm25: 0.4,
            fuzzy: 0.1,
        };

        let hybrid = engine.search_hybrid("vector search", &semantic_results, &weights, 10);
        assert!(!hybrid.is_empty());
    }

    #[test]
    fn test_hybrid_weights_default() {
        let weights = HybridWeights::default();
        assert!((weights.semantic - 0.6).abs() < 0.001);
        assert!((weights.bm25 - 0.3).abs() < 0.001);
        assert!((weights.fuzzy - 0.1).abs() < 0.001);
    }

    // ========================================================================
    // Typo Correction Tests
    // ========================================================================

    #[test]
    fn test_correct_word_exact_match() {
        let mut index = FuzzyIndex::new();
        index.add_document("doc1", "programming language");

        // Exact match should return the word with distance 0
        let result = index.correct_word("programming");
        assert!(result.is_some());
        let (corrected, distance) = result.unwrap();
        assert_eq!(corrected, "programming");
        assert_eq!(distance, 0);
    }

    #[test]
    fn test_correct_word_with_typo() {
        let mut index = FuzzyIndex::new();
        index.add_document("doc1", "programming language");

        // "progamming" is 1 edit away from "programming" (missing 'r')
        let result = index.correct_word("progamming");
        assert!(result.is_some());
        let (corrected, distance) = result.unwrap();
        assert_eq!(corrected, "programming");
        assert_eq!(distance, 1);
    }

    #[test]
    fn test_correct_word_no_match() {
        let mut index = FuzzyIndex::new();
        index.add_document("doc1", "programming language");

        // "xyz" is too far from any vocabulary word
        let result = index.correct_word("xyz");
        assert!(result.is_none());
    }

    #[test]
    fn test_correct_query_single_typo() {
        let mut index = FuzzyIndex::new();
        index.add_document("doc1", "rust programming language");

        let (corrected, corrections) = index.correct_query("progamming");
        assert_eq!(corrected, "programming");
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].original, "progamming");
        assert_eq!(corrections[0].corrected, "programming");
        assert_eq!(corrections[0].distance, 1);
    }

    #[test]
    fn test_correct_query_multiple_typos() {
        let mut index = FuzzyIndex::new();
        index.add_document("doc1", "rust programming language");

        // "progamming languge" has typos in both words
        let (corrected, corrections) = index.correct_query("progamming languge");
        assert_eq!(corrected, "programming language");
        assert_eq!(corrections.len(), 2);
    }

    #[test]
    fn test_correct_query_no_typos() {
        let mut index = FuzzyIndex::new();
        index.add_document("doc1", "rust programming language");

        // No typos - should return same query with empty corrections
        let (corrected, corrections) = index.correct_query("programming language");
        assert_eq!(corrected, "programming language");
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_search_bm25_with_correction() {
        let mut engine = SearchEngine::new();

        let docs = vec![
            Document {
                id: "doc1".to_string(),
                content: "Rust is a systems programming language".to_string(),
                metadata: Default::default(),
                embedding: None,
            },
            Document {
                id: "doc2".to_string(),
                content: "Python is popular for scripting".to_string(),
                metadata: Default::default(),
                embedding: None,
            },
        ];

        engine.index_documents(&docs);

        // Search with typo "progamming" (missing 'r')
        let (results, corrected_query, corrections) =
            engine.search_bm25_with_correction("progamming", 10);

        // Should find doc1 after correcting to "programming"
        assert!(!results.is_empty());
        assert_eq!(results[0].0, "doc1");
        assert_eq!(corrected_query, "programming");
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].original, "progamming");
        assert_eq!(corrections[0].corrected, "programming");
    }

    #[test]
    fn test_vocabulary_cleared() {
        let mut index = FuzzyIndex::new();
        index.add_document("doc1", "programming language");

        assert!(index.vocabulary_size() > 0);

        index.clear();

        assert_eq!(index.vocabulary_size(), 0);
        assert!(index.is_empty());
    }

    #[test]
    fn test_typo_correction_case_insensitive() {
        let mut index = FuzzyIndex::new();
        index.add_document("doc1", "Programming Language");

        // Query in different case with typo
        let result = index.correct_word("PROGAMMING");
        assert!(result.is_some());
        let (corrected, _) = result.unwrap();
        assert_eq!(corrected, "programming"); // lowercase
    }

    // ========================================================================
    // Persistence Tests
    // ========================================================================

    #[test]
    fn test_bm25_save_load() {
        let temp_dir = std::env::temp_dir().join("ares_test_bm25");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let path = temp_dir.join("bm25_index.json");

        // Create and populate index
        let mut index = Bm25Index::new();
        index.add_document("doc1", "The quick brown fox");
        index.add_document("doc2", "A lazy dog sleeps");
        assert_eq!(index.len(), 2);

        // Save to disk
        index.save(&path).unwrap();

        // Load from disk
        let loaded = Bm25Index::load(&path).unwrap();
        assert_eq!(loaded.len(), 2);

        // Verify search still works
        let results = loaded.search("quick brown", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, "doc1");

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_fuzzy_save_load() {
        let temp_dir = std::env::temp_dir().join("ares_test_fuzzy");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let path = temp_dir.join("fuzzy_index.json");

        // Create and populate index
        let mut index = FuzzyIndex::new();
        index.add_document("doc1", "machine learning algorithms");
        index.add_document("doc2", "deep neural networks");
        assert_eq!(index.len(), 2);

        // Save to disk
        index.save(&path).unwrap();

        // Load from disk
        let loaded = FuzzyIndex::load(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.vocabulary_size(), index.vocabulary_size());

        // Verify search still works
        let results = loaded.search("machine", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, "doc1");

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_search_engine_save_load() {
        let temp_dir = std::env::temp_dir().join("ares_test_engine");
        let _ = std::fs::remove_dir_all(&temp_dir);

        // Create and populate engine
        let mut engine = SearchEngine::new();
        let docs = vec![
            Document {
                id: "doc1".to_string(),
                content: "Rust programming language".to_string(),
                metadata: Default::default(),
                embedding: None,
            },
            Document {
                id: "doc2".to_string(),
                content: "Python scripting language".to_string(),
                metadata: Default::default(),
                embedding: None,
            },
        ];
        engine.index_documents(&docs);
        assert_eq!(engine.len(), 2);

        // Save to disk
        engine.save(&temp_dir).unwrap();

        // Load from disk
        let loaded = SearchEngine::load(&temp_dir).unwrap();
        assert_eq!(loaded.len(), 2);

        // Verify BM25 search still works
        let bm25_results = loaded.search_bm25("Rust programming", 10);
        assert!(!bm25_results.is_empty());
        assert_eq!(bm25_results[0].0, "doc1");

        // Verify fuzzy search still works
        let fuzzy_results = loaded.search_fuzzy("rust", 10);
        assert!(!fuzzy_results.is_empty());

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_load_or_new_missing_file() {
        let path = std::env::temp_dir().join("nonexistent_bm25_index.json");
        let _ = std::fs::remove_file(&path); // Ensure it doesn't exist

        // Should return empty index
        let index = Bm25Index::load_or_new(&path);
        assert!(index.is_empty());
    }

    #[test]
    fn test_search_engine_load_or_new() {
        let temp_dir = std::env::temp_dir().join("ares_test_load_or_new");
        let _ = std::fs::remove_dir_all(&temp_dir); // Ensure it doesn't exist

        // Should return empty engine
        let engine = SearchEngine::load_or_new(&temp_dir);
        assert!(engine.is_empty());
    }
}
