# Changelog

All notable changes to A.R.E.S will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.2] - 2026-01-28

### Added

- **Query-Level Typo Correction**: Fuzzy search now corrects typos in search queries
  - `QueryCorrection` struct for vocabulary-based word correction
  - `correct_word()` and `correct_query()` methods using Levenshtein distance
  - `search_bm25_with_correction()` and `search_hybrid_with_correction()` methods
  - Vocabulary built from indexed documents for domain-specific corrections
  - Location: `src/rag/search.rs`
  - Closes GitHub issue #4

- **Embedding Cache**: In-memory LRU cache for embedding vectors
  - `EmbeddingCache` trait with `get/set/invalidate/clear/stats` methods
  - `LruEmbeddingCache` implementation with SHA-256 hashing, configurable max entries, optional TTL
  - `CachedEmbeddingService` wrapper for transparent caching
  - Thread-safe with `parking_lot` RwLock
  - 12 comprehensive tests
  - Location: `src/rag/cache.rs`

### Changed

- Updated documentation to reflect implemented features
  - `KNOWN_ISSUES.md`: Marked fuzzy search typo issue as resolved
  - `DIR-24_RAG_IMPLEMENTATION_PLAN.md`: Marked embedding cache as implemented
  - `FUTURE_ENHANCEMENTS.md`: Updated embedding cache section
  - `README.md`: Updated version reference

### Removed

- Stale session log file (`session-ses_43bd.md`)

## [0.3.1] - 2026-01-16

### Fixed

- **Vector Persistence (CRITICAL)**: Fixed bug where vectors were not saved to disk
  - Root cause: HNSW index didn't support iteration, so `save_collection()` saved empty files
  - Added `export_all()` method to `HnswIndex` in `crates/ares-vector/src/index.rs`
  - Added `export_all()` method to `Collection` in `crates/ares-vector/src/collection.rs`
  - Updated `save_collection()` in `crates/ares-vector/src/persistence.rs` to actually save vectors
  - Added regression tests: `test_vector_persistence_regression` and `test_metadata_persistence`

- **Race Condition in Parallel Model Loading (MEDIUM)**: Fixed concurrent download failures
  - Root cause: Multiple threads loading fastembed model simultaneously caused conflicts
  - Added per-model initialization locks using `OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>>`
  - Applied locks to `EmbeddingService::new()`, `embed_texts()`, and `embed_sparse()`
  - Location: `src/rag/embeddings.rs`

### Known Issues

- **Fuzzy Search with Query Typos (LOW)**: Query "progamming languge" returns 0 results
  - See GitHub issue #4 for details and proposed fix
  - Workaround: Use semantic search or spell queries correctly

## [0.3.0] - 2026-01-13

### Added

- **ares-vector**: Pure-Rust vector database with HNSW indexing
  - No external dependencies (Qdrant, Milvus, etc. not required)
  - Memory-mapped persistence via `memmap2`
  - Multiple distance metrics: Cosine, Euclidean, Dot Product
  - Thread-safe with `parking_lot` RwLocks
  - Collection management (create, delete, list)
  - Located in `crates/ares-vector/`

- **RAG Pipeline**: Comprehensive document retrieval system
  - Document ingestion with automatic chunking
  - Multiple chunking strategies: word, character, semantic
  - Configurable chunk size and overlap

- **Embedding Service**: Multi-model embedding support
  - BGE family (small, base, large) via FastEmbed
  - All-MiniLM models (L6, L12)
  - Nomic Embed Text v1.5
  - Qwen3 Embeddings (via Candle)
  - GTE-Modern-BERT (via Candle)
  - Sparse embeddings (SPLADE) for hybrid search

- **Multi-Strategy Search**: Multiple search algorithms
  - Semantic: Vector similarity search
  - BM25: Traditional TF-IDF keyword matching
  - Fuzzy: Levenshtein distance for typo tolerance
  - Hybrid: Weighted combination of semantic + BM25

- **Reranking**: Cross-encoder reranking for improved relevance
  - MiniLM-L6-v2 cross-encoder
  - BGE Reranker support

- **RAG API Endpoints**:
  - `POST /api/rag/ingest` - Ingest documents with chunking
  - `POST /api/rag/search` - Multi-strategy search with optional reranking
  - `GET /api/rag/collections` - List all collections
  - `DELETE /api/rag/collections/{name}` - Delete a collection

- **New feature flag**: `ares-vector` for pure-Rust vector store

### Changed

- **CI Workflow**: Added `ares-vector` feature to test matrix across all platforms
- **Feature flags**: Now 15+ feature flags (was 12+)

## [0.2.5] - 2024-12-21

### Changed

- **Swagger UI is now optional**: The interactive API documentation (Swagger UI) is now behind the `swagger-ui` feature flag
  - This reduces the default binary size and build time
  - The core server no longer requires network access during build
  - Enable with `cargo build --features swagger-ui` or use the `full` bundle
  - When enabled, Swagger UI is available at `/swagger-ui/`

- **Improved docs.rs compatibility**: Documentation builds now work on docs.rs
  - Removed problematic dependencies from docs.rs builds (`llamacpp`, `qdrant`, `swagger-ui`)
  - These features require native compilation or network access which docs.rs doesn't support

### Fixed

- **docs.rs build failures**: Fixed build failures caused by:
  - `utoipa-swagger-ui` requiring network access to download Swagger UI assets
  - `llama-cpp-sys-2` requiring native C++ compilation
  - `qdrant-client` build script requiring filesystem write access

## [0.2.4] - 2024-12-21

### Fixed

- **CI workflow**: Fixed rust-cache key validation errors caused by commas in feature matrix
- **Clippy errors**: Fixed various clippy warnings treated as errors in CI
- **Test compilation**: Fixed `ChatCompletionTools` enum pattern matching in OpenAI tests

## 0.2.3

### Added

- **CLI Commands**: Full-featured command-line interface with colored TUI output
  - `ares-server init` - Scaffold a new A.R.E.S project with all configuration files
  - `ares-server config` - View and validate configuration
  - `ares-server agent list` - List all configured agents
  - `ares-server agent show <name>` - Show details for a specific agent
  - Global options: `--config`, `--verbose`, `--no-color`
  - Init options: `--force`, `--minimal`, `--no-examples`, `--provider`, `--host`, `--port`

- **Embedded Web UI**: Leptos-based frontend that can be bundled with the backend
  - New `ui` feature flag to embed the UI in the server binary
  - New `full-ui` feature bundle (all features + UI)
  - UI served at `/` when enabled
  - SPA routing support for client-side navigation

- **Node.js Runtime Detection**: Build-time check for bun, npm, or deno when UI feature is enabled

- **CLI Integration Tests**: Comprehensive test suite for all CLI commands
  - Unit tests for output formatting
  - Unit tests for init scaffolding
  - Integration tests for command execution

### Changed

- **Installation Experience**: Users can now run `ares-server init` after installing via `cargo install`
  - No longer requires cloning the repository to get started
  - Auto-generates `ares.toml`, `.env.example`, and all TOON configuration files
  - Creates directory structure: `data/`, `config/agents/`, `config/models/`, etc.

- **Justfile**: Added new commands
  - `just init` - Initialize project using CLI
  - `just build-ui` - Build with embedded UI (auto-detects Node.js runtime)
  - `just build-full-ui` - Build with all features including UI
  - `just run-ui` - Run server with UI feature
  - `just check-node` - Check for available Node.js runtime

- **CI Workflow**: Updated to include CLI tests and UI builds
  - New `cli-tests` job for CLI integration tests
  - New `build-ui` job for UI feature compilation
  - Tests run on all supported platforms

- **Dockerfile**: Updated for new CLI and binary name
  - Multi-stage build with UI support
  - Non-root user for improved security
  - Proper binary name (`ares-server`)

- **Documentation**: Comprehensive updates
  - README.md: Added CLI commands, UI feature, troubleshooting, requirements sections
  - QUICK_REFERENCE.md: Added CLI quick reference
  - Added CHANGELOG.md

### Fixed

- Configuration loading no longer requires environment variables for info commands
  - `ares-server config` works without JWT_SECRET set
  - `ares-server agent list/show` works without environment variables

## [0.2.2] - 2024-12-20

### Added

- Hot-reload configuration support for `ares.toml`
- TOON format support for agent, model, tool, and workflow configurations
- Dynamic configuration manager for runtime config changes
- Per-agent tool filtering

### Changed

- Improved error messages for configuration validation
- Better handling of missing configuration files

## [0.2.1] - 2024-12-15

### Added

- Workflow engine for multi-agent orchestration
- Deep research endpoint with parallel subagents
- MCP (Model Context Protocol) server support

### Fixed

- Memory management for long conversations
- Token counting accuracy for streaming responses

## [0.2.0] - 2024-12-10

### Added

- Multi-provider LLM support (Ollama, OpenAI, LlamaCpp)
- Tool calling with automatic schema generation
- JWT-based authentication
- Swagger UI for API documentation
- RAG support with semantic search
- Web search tool (no API key required)

### Changed

- Migrated to Axum web framework
- Improved streaming response handling
- Better error handling and logging

## [0.1.0] - 2024-12-01

### Added

- Initial release
- Basic chat functionality with Ollama
- SQLite database support
- Simple agent framework
- REST API endpoints

---

[0.3.2]: https://github.com/dirmacs/ares/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/dirmacs/ares/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/dirmacs/ares/compare/v0.2.5...v0.3.0
[0.2.5]: https://github.com/dirmacs/ares/compare/v0.2.4...v0.2.5
[0.2.4]: https://github.com/dirmacs/ares/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/dirmacs/ares/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/dirmacs/ares/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/dirmacs/ares/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/dirmacs/ares/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/dirmacs/ares/releases/tag/v0.1.0
