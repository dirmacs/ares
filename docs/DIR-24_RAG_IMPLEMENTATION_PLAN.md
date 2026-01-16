# DIR-24: AI-native protocols, Vector DBs, RAG, and Search Strategies

**Linear Issue**: [DIR-24](https://linear.app/dirmacs/issue/DIR-24/ai-native-protocols-vector-dbs-rag-and-search-strategies)  
**Parent Issue**: DIR-12 (anyone make agent on ares)  
**Related Issue**: DIR-25 (public GitHub issue)  
**Status**: ✅ Complete (Core Implementation)  
**Priority**: Urgent  
**Created**: 2026-01-03  
**Last Updated**: 2026-01-16  

---

## Implementation Status

### Completed Features ✅

| Feature | Status | Location |
|---------|--------|----------|
| **ares-vector crate** | ✅ Done | `crates/ares-vector/` |
| **HNSW indexing** | ✅ Done | `crates/ares-vector/src/index.rs` |
| **Embedding service** | ✅ Done | `src/rag/embeddings.rs` |
| **Chunking strategies** | ✅ Done | `src/rag/chunker.rs` |
| **Multi-strategy search** | ✅ Done | `src/rag/search.rs` |
| **Reranking** | ✅ Done | `src/rag/reranker.rs` |
| **RAG API endpoints** | ✅ Done | `src/api/handlers/rag.rs` |

### API Endpoints (Live)

- `POST /api/rag/ingest` - Ingest documents with chunking
- `POST /api/rag/search` - Multi-strategy search (semantic, BM25, fuzzy, hybrid)
- `GET /api/rag/collections` - List collections
- `DELETE /api/rag/collection` - Delete collection

### Deferred to Future Iterations

- Additional vector DB providers (pgvector, ChromaDB, Pinecone)
- GPU acceleration for embeddings
- Embedding cache layer
- AI-native protocols (ACP, AG-UI, ANP, A2A)

---

## Executive Summary

This document outlines the implementation plan for a production-ready RAG (Retrieval Augmented Generation) system in ARES with:

- **Multi-provider vector database support** (LanceDB default, + Qdrant, pgvector, ChromaDB, Pinecone)
- **Multiple search strategies** (semantic, BM25, fuzzy, hybrid)
- **Reranking pipeline** (BGE, Jina rerankers)
- **30+ embedding models** (BGE, Qwen3, Gemma, E5, Jina, etc.)
- **Configurable via `ares.toml`** with live/mocked test infrastructure

**Note**: AI-native protocols (ACP, AG-UI, ANP, A2A) are deferred to a future iteration pending more research.

---

## Table of Contents

1. [Current State Analysis](#current-state-analysis)
2. [Research Findings](#research-findings)
3. [Implementation Plan](#implementation-plan)
4. [Dependencies](#dependencies)
5. [Feature Flags](#feature-flags)
6. [Configuration Schema](#configuration-schema)
7. [API Endpoints](#api-endpoints)
8. [Test Infrastructure](#test-infrastructure)
9. [Deferred Features](#deferred-features)
10. [References](#references)

---

## Current State Analysis

### What Exists (as of 2026-01-12)

| Component | Status | Location | Notes |
|-----------|--------|----------|-------|
| **Qdrant Client** | ⚠️ Partial | `src/db/qdrant.rs` | Has placeholder at line 91 - query embedding not wired |
| **Embeddings** | ✅ Basic | `src/rag/embeddings.rs` | Uses `fastembed` 5.5.0 with BGE-small-en-v1.5 |
| **Chunker** | ⚠️ Basic | `src/rag/chunker.rs` | Simple word-based, no semantic awareness |
| **MCP Server** | ✅ Done | `src/mcp/server.rs` | Full implementation with tools |
| **RAG API** | ❌ Missing | - | No `/rag/*` endpoints |
| **Search Strategies** | ❌ None | - | Only basic Qdrant semantic (not wired) |
| **Reranking** | ❌ None | - | Not implemented |
| **Other Vector DBs** | ❌ None | - | Only Qdrant |

### Critical Gap: Qdrant Query Embedding

```rust
// src/db/qdrant.rs line 91-93 - THIS IS A PLACEHOLDER
let mut search_builder = SearchPointsBuilder::new(
    collection_name,
    vec![], // Placeholder - needs actual query embedding
    query.limit as u64,
)
```

---

## Research Findings

### Vector Database Crates

| Crate | Version | Official | Local-First | Cloud | Status | Recommendation |
|-------|---------|----------|-------------|-------|--------|----------------|
| `lancedb` | 0.23.1 | ✅ Yes | ✅✅ Serverless | ✅ | Production | **Default** |
| `qdrant-client` | 1.16.0 | ✅ Yes | ✅ | ✅ | Production | Keep existing |
| `pgvector` | 0.4.1 | Community | ✅ | ✅ | Production | Add |
| `chromadb` | 2.3.0 | Community | ✅ | ✅ | Stable | Add |
| `pinecone-sdk` | 0.1.2 | ✅ Yes | ❌ | ✅ | Alpha | Add (cloud-only) |
| `milvus` | 0.2.0 | ❌ | - | - | Abandoned | **Skip** |
| `weaviate` | 0.1.0 | ✅ Yes | - | - | Not ready | **Skip** |

**Why LanceDB as default?**
- Truly serverless (no separate process needed)
- Embedded database (like SQLite for vectors)
- Local-first by design
- Good performance
- Active development

### Embedding Models (via FastEmbed 5.8.1)

#### Text Embedding Models

| Model | Dimensions | Multilingual | Use Case |
|-------|------------|--------------|----------|
| `BGESmallENV15` | 384 | No | Fast English (default) |
| `BGESmallENV15Q` | 384 | No | Fast English, quantized |
| `BGEBaseENV15` | 768 | No | Balanced English |
| `BGELargeENV15` | 1024 | No | High quality English |
| `BGEM3` | 1024 | ✅ 100+ langs | Best multilingual, 8192 context |
| `AllMiniLML6V2` | 384 | No | Lightweight |
| `AllMiniLML12V2` | 384 | No | Better quality |
| `MultilingualE5Small` | 384 | ✅ | Small multilingual |
| `MultilingualE5Base` | 768 | ✅ | Base multilingual |
| `MultilingualE5Large` | 1024 | ✅ | Large multilingual |
| `NomicEmbedTextV15` | 768 | No | 8192 context |
| `GTEBaseENV15` | 768 | No | Alibaba GTE |
| `GTELargeENV15` | 1024 | No | Alibaba GTE large |
| `MxbaiEmbedLargeV1` | 1024 | No | High quality |
| `ModernBertEmbedLarge` | 1024 | No | Modern BERT (2024) |
| `EmbeddingGemma300M` | 768 | No | Google Gemma |
| `JinaEmbeddingsV2BaseCode` | 768 | No | Code-optimized |
| `JinaEmbeddingsV2BaseEN` | 768 | No | Jina English |
| `SnowflakeArcticEmbed*` | 384-1024 | No | Various sizes |
| `ClipVitB32` | 512 | No | Image-text (CLIP) |

#### Qwen3 Embedding Models (Feature: `qwen3`)

| Model | Dimensions | Notes |
|-------|------------|-------|
| `Qwen3-Embedding-0.6B` | 1024 | Small, efficient |
| `Qwen3-Embedding-4B` | 2560 | Medium, high quality |
| `Qwen3-Embedding-8B` | 4096 | Largest, best quality |

**Usage requires Candle backend:**
```toml
fastembed = { version = "5.8.1", features = ["qwen3"] }
```

#### Sparse Embedding Models (for Hybrid Search)

| Model | Type | Notes |
|-------|------|-------|
| `SPLADEPPV1` | SPLADE | English sparse embeddings |
| `BGEM3` | Sparse | Multilingual sparse |

#### Reranking Models

| Model | Multilingual | Notes |
|-------|--------------|-------|
| `BGERerankerBase` | No | Default |
| `BGERerankerV2M3` | ✅ | Multilingual v2 |
| `JINARerankerV1TurboEn` | No | Fast English |
| `JINARerankerV2BaseMultilingual` | ✅ | Multilingual |

### Search & Text Processing Crates

| Crate | Version | Purpose | Notes |
|-------|---------|---------|-------|
| `bm25` | 2.3.2 | Keyword search | BM25 scoring, multilingual |
| `strsim` | 0.11.1 | Fuzzy matching | Levenshtein, Jaro-Winkler, etc. |
| `simsimd` | 6.5.12 | Vector distance | SIMD-optimized, 10-100x faster |
| `tantivy` | 0.25.0 | Full-text search | Lucene-like (future) |
| `text-splitter` | 0.29.3 | Chunking | Semantic, markdown, code-aware |
| `fuzzy-matcher` | 0.3.7 | FZF-style matching | Smith-Waterman based |

### Patterns from Donda (Related Project)

The `donda` project (repomix in `docs/`) provides useful patterns:

```rust
// Ingestor pattern: chunker → embedder → storage
pub struct Ingestor {
    pool: PgPool,
    chunker: Chunker,
    embedder: Embedder,
}

// SQLx with pgvector
sqlx = { version = "0.8.6", features = ["runtime-tokio", "postgres", "migrate"] }
pgvector = { version = "0.4.1", features = ["sqlx"] }

// Chunk model
pub struct Chunk {
    pub id: i32,
    pub text: String,
    pub source: String,
    pub index: i32,
    pub metadata: serde_json::Value,
    pub embedding: Option<Vec<f32>>,
}
```

**Key insight**: Use `runtime-tokio` for SQLx (confirmed compatible with existing ARES tokio usage).

---

## Implementation Plan

### Phase 1: Core Infrastructure

#### Step 1: VectorStore Trait (`src/db/vectorstore.rs`)

```rust
use async_trait::async_trait;
use crate::types::{Document, SearchQuery, SearchResult, Result};

/// Configuration for vector store providers
#[derive(Debug, Clone)]
pub enum VectorStoreProvider {
    LanceDB { path: String },
    Qdrant { url: String, api_key: Option<String> },
    PgVector { connection_string: String },
    ChromaDB { url: String },
    Pinecone { api_key: String, environment: String, index_name: String },
}

/// Abstract trait for vector database operations
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Create a new collection/index
    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()>;
    
    /// List all collections
    async fn list_collections(&self) -> Result<Vec<String>>;
    
    /// Upsert documents with embeddings
    async fn upsert(&self, collection: &str, documents: &[Document]) -> Result<()>;
    
    /// Search by vector similarity
    async fn search(&self, collection: &str, query: &SearchQuery, embedding: &[f32]) -> Result<Vec<SearchResult>>;
    
    /// Delete documents by ID
    async fn delete(&self, collection: &str, ids: &[String]) -> Result<()>;
    
    /// Get collection statistics
    async fn stats(&self, collection: &str) -> Result<CollectionStats>;
}

pub struct CollectionStats {
    pub document_count: usize,
    pub dimensions: usize,
    pub index_size_bytes: Option<u64>,
}
```

#### Step 2: Implement Providers

| Provider | File | Priority |
|----------|------|----------|
| LanceDB | `src/db/lancedb.rs` | P0 (default) |
| Qdrant | `src/db/qdrant.rs` | P0 (fix existing) |
| pgvector | `src/db/pgvector.rs` | P1 |
| ChromaDB | `src/db/chromadb.rs` | P1 |
| Pinecone | `src/db/pinecone.rs` | P2 |

#### Step 3: Upgrade Embedding Service (`src/rag/embeddings.rs`)

```rust
use tokio::task::spawn_blocking;

pub enum EmbeddingModel {
    // Fast English
    BGESmallENV15,
    BGESmallENV15Q,
    // High quality English
    BGEBaseENV15,
    BGELargeENV15,
    // Multilingual
    BGEM3,
    MultilingualE5Small,
    MultilingualE5Base,
    MultilingualE5Large,
    // Specialized
    JinaEmbeddingsV2BaseCode,
    EmbeddingGemma300M,
    // ... 20+ more
}

pub struct EmbeddingService {
    model: TextEmbedding,
    sparse_model: Option<SparseTextEmbedding>,
    batch_size: usize,
}

impl EmbeddingService {
    pub async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let model = self.model.clone();
        let batch_size = self.batch_size;
        
        // Use spawn_blocking for sync fastembed calls
        spawn_blocking(move || {
            let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
            model.embed(refs, Some(batch_size))
                .map_err(|e| AppError::Internal(e.to_string()))
        }).await?
    }
    
    pub async fn embed_sparse(&self, texts: Vec<String>) -> Result<Vec<SparseEmbedding>> {
        // For hybrid search
        // ...
    }
}
```

#### Step 4: Search Strategies (`src/rag/search.rs`)

```rust
pub enum SearchStrategy {
    Semantic,           // Vector similarity only
    BM25,               // Keyword ranking only
    Fuzzy,              // String similarity
    Hybrid {            // Combined semantic + BM25
        semantic_weight: f32,
        keyword_weight: f32,
    },
}

impl Default for SearchStrategy {
    fn default() -> Self {
        SearchStrategy::Hybrid {
            semantic_weight: 0.7,
            keyword_weight: 0.3,
        }
    }
}

pub struct SearchEngine {
    vector_store: Arc<dyn VectorStore>,
    embedding_service: Arc<EmbeddingService>,
    bm25_index: Option<BM25Index>,
    reranker: Option<Reranker>,
}

impl SearchEngine {
    pub async fn search(
        &self,
        query: &str,
        strategy: SearchStrategy,
        top_k: usize,
        rerank: bool,
    ) -> Result<Vec<SearchResult>> {
        let results = match strategy {
            SearchStrategy::Semantic => self.semantic_search(query, top_k).await?,
            SearchStrategy::BM25 => self.bm25_search(query, top_k).await?,
            SearchStrategy::Fuzzy => self.fuzzy_search(query, top_k).await?,
            SearchStrategy::Hybrid { semantic_weight, keyword_weight } => {
                self.hybrid_search(query, top_k, semantic_weight, keyword_weight).await?
            }
        };
        
        if rerank {
            self.rerank(query, results).await
        } else {
            Ok(results)
        }
    }
}
```

#### Step 5: Reranking (`src/rag/reranker.rs`)

```rust
use fastembed::{TextRerank, RerankInitOptions, RerankerModel};
use tokio::task::spawn_blocking;

pub struct Reranker {
    model: TextRerank,
    top_n: usize,
}

impl Reranker {
    pub fn new(model: RerankerModel, top_n: usize) -> Result<Self> {
        let model = TextRerank::try_new(RerankInitOptions::new(model))?;
        Ok(Self { model, top_n })
    }
    
    pub async fn rerank(&self, query: &str, documents: Vec<SearchResult>) -> Result<Vec<SearchResult>> {
        let query = query.to_string();
        let top_n = self.top_n;
        let model = self.model.clone();
        
        spawn_blocking(move || {
            let doc_texts: Vec<&str> = documents.iter().map(|d| d.document.content.as_str()).collect();
            let rankings = model.rerank(&query, doc_texts, true, Some(top_n))?;
            // Reorder documents based on rankings
            // ...
        }).await?
    }
}
```

#### Step 6: Upgrade Chunking (`src/rag/chunker.rs`)

```rust
use text_splitter::{TextSplitter, ChunkConfig, MarkdownSplitter, CodeSplitter};

pub enum ChunkingStrategy {
    Fixed { size: usize, overlap: usize },
    Semantic { max_tokens: usize },
    Markdown { max_tokens: usize },
    Code { language: String, max_tokens: usize },
}

pub struct Chunker {
    strategy: ChunkingStrategy,
}

impl Chunker {
    pub fn chunk(&self, text: &str) -> Result<Vec<Chunk>> {
        match &self.strategy {
            ChunkingStrategy::Fixed { size, overlap } => self.fixed_chunk(text, *size, *overlap),
            ChunkingStrategy::Semantic { max_tokens } => {
                let splitter = TextSplitter::new(*max_tokens);
                self.split_with(text, splitter)
            }
            ChunkingStrategy::Markdown { max_tokens } => {
                let splitter = MarkdownSplitter::new(*max_tokens);
                self.split_with(text, splitter)
            }
            ChunkingStrategy::Code { language, max_tokens } => {
                let splitter = CodeSplitter::new(language, *max_tokens)?;
                self.split_with(text, splitter)
            }
        }
    }
}
```

### Phase 2: API Endpoints

#### Step 7: RAG API Handlers (`src/api/handlers/rag.rs`)

```rust
use axum::{extract::{State, Path, Json}, response::IntoResponse};

/// POST /rag/ingest - Ingest documents
pub async fn ingest_documents(
    State(state): State<AppState>,
    Json(request): Json<IngestRequest>,
) -> Result<Json<IngestResponse>, AppError> {
    // 1. Chunk documents
    // 2. Generate embeddings (batch)
    // 3. Store in vector DB
    // 4. Update BM25 index
}

/// POST /rag/search - Search with strategy selection
pub async fn search(
    State(state): State<AppState>,
    Json(request): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, AppError> {
    // 1. Parse strategy from request
    // 2. Execute search
    // 3. Optionally rerank
    // 4. Return results with sources
}

/// GET /rag/collections - List collections
pub async fn list_collections(
    State(state): State<AppState>,
) -> Result<Json<Vec<CollectionInfo>>, AppError> { ... }

/// DELETE /rag/documents/{id} - Remove document
pub async fn delete_document(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> { ... }

/// GET /rag/stats - Get statistics
pub async fn stats(
    State(state): State<AppState>,
) -> Result<Json<RagStats>, AppError> { ... }
```

#### Step 8: Update Routes (`src/api/routes.rs`)

```rust
// Add to protected_routes
.route("/rag/ingest", post(crate::api::handlers::rag::ingest_documents))
.route("/rag/search", post(crate::api::handlers::rag::search))
.route("/rag/collections", get(crate::api::handlers::rag::list_collections))
.route("/rag/documents/{id}", delete(crate::api::handlers::rag::delete_document))
.route("/rag/stats", get(crate::api::handlers::rag::stats))
```

---

## Dependencies

### New Dependencies (Cargo.toml)

```toml
[dependencies]
# Vector DBs
lancedb = { version = "0.23.1", optional = true }
pgvector = { version = "0.4.1", features = ["sqlx"], optional = true }
sqlx = { version = "0.8.6", features = ["runtime-tokio", "postgres"], optional = true }
chromadb = { version = "2.3.0", optional = true }
pinecone-sdk = { version = "0.1.2", optional = true }

# Search
bm25 = { version = "2.3.2", features = ["parallelism", "language_detection"] }
strsim = "0.11.1"
simsimd = "6.5.12"

# Chunking (replaces simple word-based chunker)
text-splitter = { version = "0.29.3", features = ["markdown", "code", "tokenizers"] }

# Upgrade existing fastembed
fastembed = { version = "5.8.1", optional = true }
```

### Existing Dependencies (Already in Cargo.toml)

```toml
# Already present - no changes needed
qdrant-client = { version = "1.16.0", optional = true }
tokio = { version = "1.48.0", features = ["full"] }
async-trait = "0.1.89"
```

---

## Feature Flags

```toml
[features]
# Vector DBs (local-first prioritized)
lancedb = ["dep:lancedb"]                    # Default, serverless
qdrant = ["dep:qdrant-client"]               # Existing
pgvector = ["dep:pgvector", "dep:sqlx"]      # PostgreSQL
chromadb = ["dep:chromadb"]                  # Simple local
pinecone = ["dep:pinecone-sdk"]              # Cloud-only

# Search capabilities  
bm25 = ["dep:bm25"]                          # Keyword search (default)

# Embedding features
qwen3-embeddings = ["fastembed/qwen3"]       # Qwen3 models (Candle backend)

# Bundles
all-vectordb-local = ["lancedb", "qdrant", "pgvector", "chromadb"]
all-vectordb = ["all-vectordb-local", "pinecone"]

# Updated default
default = ["local-db", "ollama", "lancedb", "bm25"]
```

---

## Configuration Schema

### TOML Configuration (`ares.toml`)

```toml
# =============================================================================
# RAG Configuration
# =============================================================================

[rag]
# Enable/disable RAG features
enabled = true

# -----------------------------------------------------------------------------
# Embedding Configuration
# -----------------------------------------------------------------------------
[rag.embedding]
# Model selection (see docs for full list)
# Options: bge-small-en-v1.5, bge-base-en-v1.5, bge-large-en-v1.5, bge-m3,
#          multilingual-e5-small, multilingual-e5-base, multilingual-e5-large,
#          qwen3-0.6b, qwen3-4b, qwen3-8b, gemma-300m, jina-code, etc.
model = "bge-small-en-v1.5"

# Embedding dimensions (auto-detected if not specified)
# dimensions = 384

# Batch size for embedding multiple documents
batch_size = 32

# Show download progress for first-time model downloads
show_download_progress = true

# -----------------------------------------------------------------------------
# Sparse Embedding (for Hybrid Search)
# -----------------------------------------------------------------------------
[rag.sparse_embedding]
enabled = true
model = "splade-pp-v1"    # or "bge-m3"

# -----------------------------------------------------------------------------
# Chunking Configuration
# -----------------------------------------------------------------------------
[rag.chunking]
# Strategy: "fixed", "semantic", "markdown", "code"
strategy = "semantic"

# Maximum chunk size in tokens
chunk_size = 512

# Overlap between chunks (for fixed strategy)
chunk_overlap = 50

# Respect sentence boundaries when splitting
respect_sentence_boundaries = true

# -----------------------------------------------------------------------------
# Search Configuration
# -----------------------------------------------------------------------------
[rag.search]
# Default strategy: "semantic", "bm25", "fuzzy", "hybrid"
default_strategy = "hybrid"

# Weights for hybrid search (must sum to 1.0)
hybrid_semantic_weight = 0.7
hybrid_keyword_weight = 0.3

# Number of results to retrieve before reranking
top_k = 10

# Minimum similarity score threshold (0.0 - 1.0)
score_threshold = 0.5

# -----------------------------------------------------------------------------
# Reranking Configuration
# -----------------------------------------------------------------------------
[rag.reranking]
enabled = true

# Model: "bge-reranker-base", "bge-reranker-v2-m3", 
#        "jina-reranker-v1-turbo-en", "jina-reranker-v2-base-multilingual"
model = "bge-reranker-v2-m3"

# Return top N results after reranking
top_n = 5

# -----------------------------------------------------------------------------
# Vector Store Configuration
# -----------------------------------------------------------------------------
[rag.vectorstore]
# Provider: "lancedb", "qdrant", "pgvector", "chromadb", "pinecone"
provider = "lancedb"

# Default collection name
collection_name = "documents"

# Provider-specific settings
[rag.vectorstore.lancedb]
# Path for LanceDB storage (relative to working directory)
path = "./data/lancedb"

[rag.vectorstore.qdrant]
url = "http://localhost:6334"
api_key_env = "QDRANT_API_KEY"    # Optional

[rag.vectorstore.pgvector]
# Connection string from environment variable
connection_string_env = "PGVECTOR_URL"

[rag.vectorstore.chromadb]
url = "http://localhost:8000"

[rag.vectorstore.pinecone]
api_key_env = "PINECONE_API_KEY"
environment = "us-east-1"
index_name = "ares-documents"
```

---

## API Endpoints

### RAG Endpoints

| Method | Endpoint | Description | Auth |
|--------|----------|-------------|------|
| `POST` | `/rag/ingest` | Ingest documents | Required |
| `POST` | `/rag/search` | Search with strategy | Required |
| `GET` | `/rag/collections` | List collections | Required |
| `DELETE` | `/rag/documents/{id}` | Delete document | Required |
| `GET` | `/rag/stats` | Get RAG statistics | Required |

### Request/Response Examples

#### Ingest Documents

```json
// POST /rag/ingest
{
  "documents": [
    {
      "id": "doc-001",
      "content": "Document text content...",
      "metadata": {
        "title": "My Document",
        "source": "manual",
        "tags": ["important", "reference"]
      }
    }
  ],
  "collection": "documents",
  "chunking_strategy": "semantic"
}

// Response
{
  "ingested": 1,
  "chunks_created": 5,
  "collection": "documents"
}
```

#### Search

```json
// POST /rag/search
{
  "query": "How do I configure authentication?",
  "strategy": "hybrid",
  "hybrid_weights": {
    "semantic": 0.7,
    "keyword": 0.3
  },
  "top_k": 10,
  "rerank": true,
  "top_n": 5,
  "collection": "documents"
}

// Response
{
  "results": [
    {
      "document": {
        "id": "doc-001",
        "content": "Authentication is configured via...",
        "metadata": { "title": "Auth Guide" }
      },
      "score": 0.92,
      "rerank_score": 0.95
    }
  ],
  "strategy_used": "hybrid",
  "total_candidates": 10,
  "reranked": true
}
```

---

## Test Infrastructure

### Mocked Tests (Default)

```rust
// tests/vectordb_tests.rs
//! Vector Database Unit Tests (Mocked)
//!
//! These tests run with mocked vector stores and don't require external services.
//! Run with: cargo test --test vectordb_tests

#[tokio::test]
async fn test_lancedb_upsert() {
    let store = MockVectorStore::new();
    // Test implementation...
}

#[tokio::test]
async fn test_search_strategies() {
    // Test semantic, BM25, fuzzy, hybrid...
}

#[tokio::test]
async fn test_reranking_pipeline() {
    // Test reranking integration...
}
```

### Live Tests (Opt-in)

```rust
// tests/vectordb_live_tests.rs
//! Live Vector Database Integration Tests
//!
//! These tests connect to REAL vector database instances.
//! They are **ignored by default**.
//!
//! # Running the tests
//!
//! ```bash
//! # LanceDB (no external server needed)
//! LANCEDB_LIVE_TESTS=1 cargo test --test vectordb_live_tests -- --ignored
//!
//! # Qdrant
//! QDRANT_LIVE_TESTS=1 QDRANT_URL=http://localhost:6334 \
//!     cargo test --test vectordb_live_tests -- --ignored
//!
//! # pgvector
//! PGVECTOR_LIVE_TESTS=1 PGVECTOR_URL=postgresql://user:pass@localhost/db \
//!     cargo test --test vectordb_live_tests -- --ignored
//!
//! # All providers
//! VECTORDB_LIVE_TESTS=1 cargo test --test vectordb_live_tests -- --ignored
//! ```

fn should_run_lancedb_tests() -> bool {
    std::env::var("LANCEDB_LIVE_TESTS").is_ok() || std::env::var("VECTORDB_LIVE_TESTS").is_ok()
}

fn should_run_qdrant_tests() -> bool {
    std::env::var("QDRANT_LIVE_TESTS").is_ok() || std::env::var("VECTORDB_LIVE_TESTS").is_ok()
}

macro_rules! skip_if_not_live {
    ($provider:expr) => {
        if !$provider() {
            eprintln!("Skipping live test. Set appropriate env var to run.");
            return;
        }
    };
}

#[tokio::test]
#[ignore]
async fn test_live_lancedb_operations() {
    skip_if_not_live!(should_run_lancedb_tests);
    // Real LanceDB tests...
}

#[tokio::test]
#[ignore]
async fn test_live_qdrant_operations() {
    skip_if_not_live!(should_run_qdrant_tests);
    // Real Qdrant tests...
}
```

---

## Deferred Features

### GPU Acceleration

**Status**: Stubs only, deferred to future iteration.

**Reason**: Requires additional research on CUDA/Metal/Vulkan integration with fastembed and ONNX runtime.

**Stub location**: `src/rag/embeddings.rs`

```rust
// TODO: GPU acceleration - see docs/FUTURE_ENHANCEMENTS.md
// Potential approach:
// - Add feature flags: `cuda`, `metal`, `vulkan`
// - Use ort execution providers for ONNX models
// - Use candle GPU features for Qwen3 models
#[allow(dead_code)]
pub enum AccelerationBackend {
    Cpu,
    Cuda { device_id: usize },
    Metal,
    Vulkan,
}
```

### Embedding Cache

**Status**: Trait stub only, no implementation.

**Reason**: Requires decisions on cache backend (Redis vs in-memory) and invalidation strategy.

**Stub location**: `src/rag/cache.rs`

```rust
//! Embedding Cache (STUB)
//!
//! This module provides infrastructure for caching embeddings to avoid
//! re-computation. Currently unimplemented.
//!
//! # Future Implementation
//!
//! Options under consideration:
//! - In-memory LRU cache (fast, limited capacity)
//! - Redis cache (distributed, persistent)
//! - Disk-based cache (large capacity, slower)
//!
//! See docs/FUTURE_ENHANCEMENTS.md for roadmap.

use async_trait::async_trait;

#[async_trait]
pub trait EmbeddingCache: Send + Sync {
    /// Get cached embedding for text hash
    async fn get(&self, hash: &str) -> Option<Vec<f32>>;
    
    /// Store embedding with text hash
    async fn set(&self, hash: &str, embedding: Vec<f32>) -> Result<(), CacheError>;
    
    /// Invalidate cached embedding
    async fn invalidate(&self, hash: &str) -> Result<(), CacheError>;
    
    /// Clear all cached embeddings
    async fn clear(&self) -> Result<(), CacheError>;
}

// Placeholder implementation that does nothing
pub struct NoOpCache;

#[async_trait]
impl EmbeddingCache for NoOpCache {
    async fn get(&self, _hash: &str) -> Option<Vec<f32>> { None }
    async fn set(&self, _hash: &str, _embedding: Vec<f32>) -> Result<(), CacheError> { Ok(()) }
    async fn invalidate(&self, _hash: &str) -> Result<(), CacheError> { Ok(()) }
    async fn clear(&self) -> Result<(), CacheError> { Ok(()) }
}
```

### AI-Native Protocols

**Status**: Not started, requires research.

**Protocols to research**:
- ACP (Agent Communication Protocol)
- AG-UI (Agent UI Protocol)
- ANP (Agent Network Protocol)
- A2A (Agent-to-Agent Protocol)

**Reason**: These are emerging standards without stable Rust implementations. MCP is already implemented.

---

## References

### Crates.io Links

- [lancedb](https://crates.io/crates/lancedb) - Serverless vector database
- [qdrant-client](https://crates.io/crates/qdrant-client) - Qdrant vector database client
- [pgvector](https://crates.io/crates/pgvector) - pgvector for Rust
- [chromadb](https://crates.io/crates/chromadb) - ChromaDB client
- [pinecone-sdk](https://crates.io/crates/pinecone-sdk) - Pinecone SDK
- [fastembed](https://crates.io/crates/fastembed) - Fast embedding library
- [bm25](https://crates.io/crates/bm25) - BM25 search
- [strsim](https://crates.io/crates/strsim) - String similarity
- [simsimd](https://crates.io/crates/simsimd) - SIMD vector operations
- [text-splitter](https://crates.io/crates/text-splitter) - Text chunking

### Related Documentation

- [ARES Project Status](./PROJECT_STATUS.md)
- [MCP Implementation](./MCP.md)
- [GGUF Usage Guide](./GGUF_USAGE.md)
- [Future Enhancements](./FUTURE_ENHANCEMENTS.md)

### External Resources

- [LanceDB Documentation](https://lancedb.github.io/lancedb/)
- [Qdrant Documentation](https://qdrant.tech/documentation/)
- [pgvector GitHub](https://github.com/pgvector/pgvector)
- [FastEmbed GitHub](https://github.com/Anush008/fastembed-rs)
- [Hugging Face Embedding Models](https://huggingface.co/models?library=sentence-transformers)

---

## Changelog

### 2026-01-12

- Initial planning document created
- Research completed for vector DBs, embedding models, search strategies
- Decision: LanceDB as default (local-first, serverless)
- Decision: Use `spawn_blocking` for async embedding (fastembed is sync)
- Decision: Use `runtime-tokio` for SQLx (confirmed compatible)
- Decision: Defer GPU acceleration and embedding cache (stubs only)
- Decision: Defer AI-native protocols (ACP, AG-UI, ANP, A2A)

---

**Document Author**: AI Copilot  
**Review Status**: Pending human review  
**Next Steps**: Begin implementation starting with `VectorStore` trait
