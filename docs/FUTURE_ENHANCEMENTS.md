# Future Enhancements

This document tracks features that are planned but deferred to future iterations. Each section includes:
- Current status (stub, not started, etc.)
- Rationale for deferral
- Implementation considerations
- Links to relevant code stubs

---

## Table of Contents

1. [GPU Acceleration for Embeddings](#gpu-acceleration-for-embeddings)
2. [Embedding Cache](#embedding-cache)
3. [AI-Native Protocols](#ai-native-protocols)
4. [Vector Store Migration Utility](#vector-store-migration-utility)
5. [Advanced Search Features](#advanced-search-features)

---

## GPU Acceleration for Embeddings

### Status: Deferred (Stubs Only)

### Rationale

GPU acceleration for embedding models would significantly improve throughput for batch document ingestion. However, it requires:

1. Platform-specific setup (CUDA drivers, Metal framework, Vulkan SDK)
2. Additional feature flags and conditional compilation
3. Testing infrastructure across multiple GPU types
4. Build complexity for CI/CD

### Implementation Considerations

#### ONNX Runtime Execution Providers

FastEmbed uses `ort` (ONNX Runtime) internally. GPU acceleration can be added via execution providers:

```rust
// Potential implementation in src/rag/embeddings.rs
use ort::{ExecutionProvider, SessionBuilder};

pub enum AccelerationBackend {
    Cpu,
    Cuda { device_id: usize },
    TensorRT { device_id: usize },
    CoreML,  // macOS
    DirectML,  // Windows
    OpenVINO,
}

impl AccelerationBackend {
    fn to_execution_providers(&self) -> Vec<ExecutionProvider> {
        match self {
            Self::Cpu => vec![ExecutionProvider::CPU(Default::default())],
            Self::Cuda { device_id } => vec![
                ExecutionProvider::CUDA(CUDAExecutionProvider::default().with_device_id(*device_id)),
                ExecutionProvider::CPU(Default::default()), // Fallback
            ],
            // ... other providers
        }
    }
}
```

#### Candle GPU Support (for Qwen3)

Qwen3 embeddings use Candle instead of ONNX. GPU support is available via features:

```toml
# Future Cargo.toml additions
candle-core = { version = "0.9", features = ["cuda"] }  # NVIDIA
candle-core = { version = "0.9", features = ["metal"] } # Apple Silicon
```

#### Feature Flags (Proposed)

```toml
[features]
# GPU acceleration (mutually exclusive per platform)
cuda = ["ort/cuda", "candle-core/cuda"]
metal = ["ort/coreml", "candle-core/metal"]
vulkan = ["ort/vulkan"]
directml = ["ort/directml"]
```

### Stub Location

- `src/rag/embeddings.rs` - `AccelerationBackend` enum (placeholder)

### References

- [ONNX Runtime Execution Providers](https://onnxruntime.ai/docs/execution-providers/)
- [Candle GPU Support](https://github.com/huggingface/candle#with-cuda-support)

---

## Embedding Cache

### Status: Implemented (In-Memory LRU)

The in-memory LRU embedding cache is now fully implemented. See `src/rag/cache.rs`.

### Current Implementation

- **Backend**: In-memory LRU cache using `parking_lot::RwLock<HashMap>`
- **Key Strategy**: SHA-256 hash of `text + model_name`
- **Eviction Policy**: LRU (Least Recently Used) with configurable max size
- **TTL Support**: Optional per-entry TTL with automatic expiry
- **Thread-safe**: Uses atomic counters and `RwLock` for concurrent access

### Usage

```rust
use ares::rag::embeddings::{CachedEmbeddingService, EmbeddingConfig};
use ares::rag::cache::CacheConfig;

// Create a cached embedding service
let service = CachedEmbeddingService::new(
    EmbeddingConfig::default(),
    CacheConfig {
        max_size_bytes: 512 * 1024 * 1024,  // 512 MB
        default_ttl: None,  // No expiry
        enabled: true,
    },
)?;

// Embeddings are automatically cached
let embedding = service.embed_text("hello world").await?;

// Check cache stats
let stats = service.cache_stats();
println!("Hit rate: {:.1}%", stats.hit_rate());
```

### Future Backend Options

Additional backends can be added by implementing the `EmbeddingCache` trait:

| Backend | Pros | Cons |
|---------|------|------|
| In-memory (LRU) | Fast, no dependencies | Limited capacity, lost on restart |
| Redis | Distributed, persistent | External dependency |
| Disk (sled/rocksdb) | Large capacity, persistent | Slower than memory |
| SQLite | Simple, persistent | May conflict with main DB |

### Configuration

```rust
use ares::rag::cache::CacheConfig;

let config = CacheConfig {
    max_size_bytes: 256 * 1024 * 1024,  // 256 MB (default)
    default_ttl: None,  // No expiry (default)
    enabled: true,  // Enabled by default
};
```

### Implementation Location

- `src/rag/cache.rs` - `EmbeddingCache` trait, `LruEmbeddingCache`, and `NoOpCache` implementations
- `src/rag/embeddings.rs` - `CachedEmbeddingService` wrapper

### Future Configuration (Proposed TOML)

```toml
[rag.cache]
enabled = true
backend = "memory"  # "memory", "redis", "disk"
max_size_mb = 512
ttl_hours = 24

# Redis-specific
[rag.cache.redis]
url = "redis://localhost:6379"
prefix = "ares:embeddings:"

# Disk-specific
[rag.cache.disk]
path = "./data/embedding_cache"
```

---

## AI-Native Protocols

### Status: Not Started (Research Required)

### Rationale

These are emerging protocols for agent-to-agent communication and UI integration. They are not yet standardized or widely adopted.

### Protocols to Research

| Protocol | Full Name | Purpose | Status |
|----------|-----------|---------|--------|
| MCP | Model Context Protocol | Tool/context sharing | ✅ Implemented |
| ACP | Agent Communication Protocol | Agent messaging | Research needed |
| AG-UI | Agent UI Protocol | UI components for agents | Research needed |
| ANP | Agent Network Protocol | Multi-agent networking | Research needed |
| A2A | Agent-to-Agent Protocol | Direct agent communication | Research needed |

### Research Questions

1. Are there official specifications for these protocols?
2. Do Rust implementations exist?
3. Which protocols are production-ready vs experimental?
4. How do they integrate with existing MCP implementation?

### Next Steps

1. Search for official documentation/specs
2. Check for Rust crates on crates.io
3. Evaluate stability and adoption
4. Prioritize based on user demand

---

## Vector Store Migration Utility

### Status: Not Started

### Rationale

As users may switch between vector store providers (e.g., from Qdrant to LanceDB), a migration utility would help preserve indexed data.

### Proposed Features

```rust
// Future: src/rag/migration.rs
pub struct VectorStoreMigration {
    source: Box<dyn VectorStore>,
    destination: Box<dyn VectorStore>,
}

impl VectorStoreMigration {
    pub async fn migrate_collection(
        &self,
        collection: &str,
        batch_size: usize,
        progress: impl Fn(MigrationProgress),
    ) -> Result<MigrationReport> {
        // 1. List all documents in source
        // 2. Batch read documents with embeddings
        // 3. Batch upsert to destination
        // 4. Verify counts match
    }
}

pub struct MigrationProgress {
    pub total: usize,
    pub completed: usize,
    pub current_batch: usize,
}

pub struct MigrationReport {
    pub documents_migrated: usize,
    pub duration: Duration,
    pub errors: Vec<MigrationError>,
}
```

### CLI Integration (Proposed)

```bash
# Future CLI command
ares-server migrate \
    --source qdrant \
    --source-url http://localhost:6334 \
    --destination lancedb \
    --destination-path ./data/lancedb \
    --collection documents \
    --batch-size 100
```

---

## Advanced Search Features

### Status: Partially Planned

### Features for Future Iterations

#### 1. Full-Text Search with Tantivy

```toml
# Future dependency
tantivy = { version = "0.25.0", optional = true }
```

Tantivy is a Lucene-like full-text search engine in Rust. It would provide:
- Advanced query parsing
- Faceted search
- Phrase queries
- Highlighting

#### 2. Approximate Nearest Neighbor (ANN) Tuning

Most vector stores support ANN algorithm tuning:

```toml
[rag.vectorstore.qdrant]
# HNSW index parameters
hnsw_m = 16              # Number of connections per layer
hnsw_ef_construct = 100  # Construction-time search width
```

#### 3. Multi-Vector Search

Support for multiple embedding types per document:

```rust
pub struct MultiVectorDocument {
    pub id: String,
    pub content: String,
    pub dense_embedding: Vec<f32>,    // Semantic
    pub sparse_embedding: SparseVec,   // BM25/SPLADE
    pub late_interaction: Vec<Vec<f32>>, // ColBERT-style
}
```

#### 4. Query Expansion

Automatic query expansion using:
- Synonyms
- LLM-generated alternatives
- Historical successful queries

```rust
pub struct QueryExpander {
    llm: Box<dyn LLMClient>,
    synonym_db: Option<SynonymDatabase>,
}

impl QueryExpander {
    pub async fn expand(&self, query: &str) -> Vec<String> {
        // Generate alternative phrasings
    }
}
```

---

## Implementation Priority

When resources become available, implement in this order:

1. ~~**Embedding Cache** (High impact, moderate effort)~~ - **DONE**
2. **GPU Acceleration** (High impact, high effort)
3. **Advanced Search** (Medium impact, varies)
4. **Migration Utility** (Low priority unless requested)
5. **AI-Native Protocols** (Pending standardization)

---

## Contributing

If you want to work on any of these features:

1. Check the stub code locations mentioned above
2. Review the implementation considerations
3. Open an issue to discuss approach before starting
4. Follow the existing patterns in the codebase

See [CONTRIBUTING.md](../CONTRIBUTING.md) for development guidelines.

---

**Last Updated**: 2026-01-28  
**Related**: [DIR-24 Implementation Plan](./DIR-24_RAG_IMPLEMENTATION_PLAN.md)
