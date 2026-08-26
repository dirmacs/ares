# Retrieval Augmented Generation

ARES ships a RAG (Retrieval Augmented Generation) system: document ingestion, chunking, embeddings, and search. This chapter describes only behavior verified in the source.

## Enabling RAG

The `[rag]` group in `ares.toml` deserializes into `RagConfig` (`crates/ares-rag/src/config.rs`). It has four sub-groups: `vector`, `chunking`, `search`, and `rerank`. Every field defaults, so an empty `[rag]` table is valid.

### Vector group (`[rag.vector]`)

| Field | Default | Meaning |
|---|---|---|
| `enabled` | false | Master switch for the RAG feature. |
| `embedding_model` | `"bge-small-en-v1.5"` | Local embedding model name. |
| `sparse_embeddings` | false | Enable sparse embeddings for hybrid search. |
| `sparse_model` | `"splade-pp-en-v1"` | Sparse model name. |
| `vector_path` | `"./data/vectors"` | Path for vector data on disk. |

### Chunking group (`[rag.chunking]`)

| Field | Default | Meaning |
|---|---|---|
| `chunking_strategy` | `"word"` | One of `"word"`, `"semantic"`, `"character"`. |
| `chunk_size` | 200 | Chunk size; words for word chunking, characters otherwise. |
| `chunk_overlap` | 50 | Overlap between consecutive chunks. |
| `min_chunk_size` | 20 | Smallest chunk size to keep, in characters. |

The chunker (`crates/ares-rag/src/chunker.rs`) exposes `TextChunker` with word, semantic, and character modes. `chunk_with_metadata` returns chunks that carry offsets.

### Search group (`[rag.search]`)

| Field | Default | Meaning |
|---|---|---|
| `search_strategy` | `"semantic"` | One of `"semantic"`, `"bm25"`, `"fuzzy"`, `"hybrid"`. |
| `search_limit` | 10 | Number of results to return. |
| `search_threshold` | 0.0 | Similarity threshold. |
| `hybrid_weights` | see below | Optional weights table. |

`hybrid_weights` takes three floats: `semantic` (default 0.5), `bm25` (default 0.3), and `fuzzy` (default 0.2). The sum should be 1.0.

### Rerank group (`[rag.rerank]`)

| Field | Default | Meaning |
|---|---|---|
| `rerank_enabled` | false | Enable reranking of retrieved results. |
| `reranker_model` | `"bge-reranker-base"` | Reranker model name. |
| `rerank_weight` | 0.6 | Weight that combines rerank and retrieval scores. |

## Ingestion paths

Two paths reach the same ingest handler:

### Command line

`ares-server rag ingest-dir` walks a local directory and posts each supported UTF-8 text file (.md, .txt, .json, .jsonl) to the server (`src/cli/rag.rs`). Flags:

| Flag | Meaning |
|---|---|
| `--host` | Server base URL. Defaults to `http://localhost:3000`. |
| `--collection` | Collection name to ingest into. Required. |
| `--docs-path` | Directory with documents. Required. |
| `--user` / `--password` | Login credentials. Used when `--token` is absent. |
| `--token` | Bearer token; skips login when present. |
| `--chunking-strategy` | `word`, `semantic`, or `character`. Defaults to `word`. |
| `--tag` | Tag to attach. Repeat for multiple tags. |
| `--dry-run` | List files without sending API requests. |

The command fails closed: a bad path or a failed login stops before any network write. Each document reports its own audit trail, so a partial run leaves no half-ingested files unrecorded.

### HTTP

`POST /api/rag/ingest` accepts a JSON body with `collection`, `content`, `title`, `source`, `tags`, and `chunking_strategy`; it returns `chunks_created` (`crates/ares-http/src/api/routes.rs`, `src/cli/rag.rs`). The endpoint requires authentication and checks tenant-level RAG source allowlists before ingesting.

Related endpoints: `POST /api/rag/search`, collection listing under `/api/rag/collections`.

## Embeddings and deduplication

Embeddings live in `crates/ares-rag/src/embeddings.rs`. Two facts matter:

- Dedup per request: identical inputs collapse by a whitespace-normalized SHA-256 content hash (`normalize_for_dedup` plus `content_hash_hex`) before the backend call. Computed vectors fan back to every duplicate slot, so callers receive full-length results while identical texts cost one backend call.
- Vectors are L2-normalized to unit length after computation (`normalize_embedding`).

The `local-embeddings` feature enables the `fastembed` backend (`crates/ares-rag/Cargo.toml`). Without it, embedding calls go over HTTP.

## Search flow

`SearchStrategy` (`crates/ares-rag/src/search.rs`) supports four strategies, also accepted by the CLI as `--strategy`:

- `semantic` — dense vector similarity through embeddings.
- `bm25` — sparse lexical matching.
- `fuzzy` — approximate string matching for typo tolerance.
- `hybrid` — combines strategies with reciprocal rank fusion weighted by `hybrid_weights`.

The BM25 and fuzzy indices support `save()` and `load()`, so they survive restarts without re-indexing. `ares-server rag search --collection <name> --query <text> [--top-k N] [--strategy S]` drives the flow from the terminal.

## Vector stores

Two backends exist behind features:

- `ares-vector` (`crates/ares-vector/`) — a pure-Rust embedded store. `VectorDb::open(Config::persistent("./data/vectors"))` loads collections from disk; `Config::memory()` runs in process. Persistence writes collection data under the configured path. This backend backs the default `[rag.vector]` path.
- Qdrant — optional external store configured through `database.qdrant` in `ares.toml` (`QdrantConfig` in `crates/ares-store/src/config.rs`). Fields: `url` (default `http://localhost:6334`) and `api_key_env`, the environment variable holding the API key.
