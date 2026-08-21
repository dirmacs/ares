# ARES RAG pipeline integration

## Overview

The ARES RAG (Retrieval-Augmented Generation) pipeline provides semantic search and context injection capabilities for document-based knowledge bases. This document describes the integration points, usage patterns, and implementation details.

## Architecture

```

                     Document Ingestion Flow                      

                                                                  
  1. Document Upload                                              
     > POST /api/rag/ingest                                     
                                                                  
  2. Chunking                                                     
     > Word Chunking (200 words, 50 overlap)                   
     > Semantic Chunking (500 words, structure-aware)          
     > Character Chunking (500 chars, 100 overlap)             
                                                                  
  3. Embedding Generation                                         
     > Local ONNX models (fastembed)                           
         > BAAI/bge-small-en-v1.5 (default, 384 dims)          
         > BAAI/bge-base-en-v1.5 (768 dims)                    
         > 30+ other models supported                          
                                                                  
  4. Vector Storage                                               
     > AresVector (HNSW, pure Rust)                            
         > Cosine similarity metric                            
         > Persistent or in-memory mode                        
         > User-scoped collections                             
                                                                  



                      Semantic Search Flow                        

                                                                  
  1. Query Embedding                                              
     > Same model as ingestion                                 
                                                                  
  2. Vector Search                                                
     > HNSW approximate nearest neighbor                       
         > Cosine similarity                                   
                                                                  
  3. Search Strategies                                            
     > Semantic (dense vectors)                                
     > BM25 (lexical, sparse)                                  
     > Fuzzy (typo-tolerant)                                   
     > Hybrid (RRF fusion of above)                            
                                                                  
  4. Reranking (optional)                                         
     > Cross-encoder models                                    
         > BGE reranker                                        
         > Jina reranker                                       
                                                                  
  5. Context Injection                                            
     > Top-k results formatted for LLM                         
                                                                  

```

## Feature flags

The RAG pipeline requires two features to be enabled:

```bash
# Build with RAG support
cargo build --features "ares-vector,local-embeddings" --no-default-features

# Full feature set
cargo build --features "ares-vector,local-embeddings,postgres,ollama,openai,mcp"
```

### Feature dependencies

- **`ares-vector`**: Pure Rust HNSW vector database
 - No native dependencies
 - Compiles anywhere Rust works
 - Persistent or in-memory storage
 
- **`local-embeddings`**: ONNX-based embedding models
 - Uses `fastembed` crate
 - Pre-downloads models via `lancor` (handles HuggingFace CDN correctly)
 - **NOT supported on Windows MSVC** (use WSL, Linux, or macOS)

## API endpoints

### Document ingestion

```http
POST /api/rag/ingest
Authorization: Bearer <jwt_token>

{
  "collection": "my_kb",
  "content": "Document text content...",
  "title": "Optional Title",
  "source": "https://example.com/doc",
  "tags": ["tag1", "tag2"],
  "chunking_strategy": "word"  # word | semantic | character
}
```

**Response:**
```json
{
  "chunks_created": 15,
  "document_ids": ["uuid_0", "uuid_1", ...],
  "collection": "my_kb"
}
```

### Semantic search

```http
POST /api/rag/search
Authorization: Bearer <jwt_token>

{
  "collection": "my_kb",
  "query": "What is the answer?",
  "limit": 5,
  "strategy": "semantic",  # semantic | bm25 | fuzzy | hybrid
  "threshold": 0.0,
  "rerank": true,
  "reranker_model": "bge-reranker-base"
}
```

**Response:**
```json
{
  "results": [
    {
      "id": "doc_chunk_3",
      "content": "Relevant text content...",
      "score": 0.87,
      "metadata": {
        "title": "Document Title",
        "source": "https://...",
        "created_at": "2026-04-13T...",
        "tags": ["tag1"]
      }
    }
  ],
  "total": 5,
  "strategy": "semantic",
  "reranked": true,
  "duration_ms": 45
}
```

### Collection management

```http
# List collections
GET /api/rag/collections
Authorization: Bearer <jwt_token>

# Delete collection
DELETE /api/rag/collection
Authorization: Bearer <jwt_token>

{
  "collection": "my_kb"
}
```

## User isolation

All RAG collections are automatically scoped per-user to prevent data leakage:

- Collection `my_kb` for user `user_123` becomes `user_123_my_kb` internally
- Users can only access their own collections
- Collection names in API responses are unscoped (user-facing)

## CLI ingestion and search

Use the generic Rust CLI for local document ingestion. The CLI has no built-in corpus paths or collection names; provide deployment-specific values explicitly or from your own wrapper outside the OSS repository.

```bash
# Preview the documents that would be ingested
ares-server rag ingest-dir \
  --host http://localhost:3000 \
  --token "$ARES_TOKEN" \
  --collection docs \
  --docs-path ./docs \
  --tag documentation \
  --dry-run

# Ingest supported UTF-8 text documents (.md, .txt, .json, .jsonl)
ares-server rag ingest-dir \
  --host http://localhost:3000 \
  --token "$ARES_TOKEN" \
  --collection docs \
  --docs-path ./docs \
  --chunking-strategy word \
  --tag documentation

# Or let the CLI obtain a bearer token through /api/auth/login
ares-server rag ingest-dir \
  --host http://localhost:3000 \
  --user user@example.com \
  --password "$ARES_PASSWORD" \
  --collection docs \
  --docs-path ./docs

# Search the collection
ares-server rag search \
  --host http://localhost:3000 \
  --token "$ARES_TOKEN" \
  --collection docs \
  --query "deployment guide" \
  --top-k 5
```

Managed deployments should keep site-specific defaults in their private wrapper repository and pass them to `ares-server rag ingest-dir`; do not add private paths, customer names, or secrets to this OSS repository.

### Context injection pattern

For LLM context injection, use the search results like this:

```rust
// After searching
let context = results
    .iter()
    .take(5)
    .map(|r| format!("[Context] (source: {}, score: {:.2})\n{}", 
        r.metadata.source, r.score, r.content))
    .collect::<Vec<_>>()
    .join("\n\n---\n\n");

// Build prompt
let prompt = format!(
    "You are a helpful assistant. Use the following context to answer the question.\n\n\
     {}\n\n\
     Question: {}\n\n\
     Answer:",
    context, user_query
);
```

## Testing

### Integration tests

Run the EHB ingestion tests:

```bash
# Enable live tests
export EHB_INGESTION_TESTS=1

# Run all EHB tests
cargo test --features "ares-vector,local-embeddings" --test rag_ehb_ingestion_tests -- --ignored

# Run specific test
cargo test --features "ares-vector,local-embeddings" --test rag_ehb_ingestion_tests \
  -- --ignored test_ehb_batch_ingestion
```

### Test coverage

The test suite covers:
- Document discovery (finds all markdown files)
- Batch ingestion (processes all documents)
- Semantic search (queries with relevance scoring)
- Context injection (formats results for LLM)
- Collection stats (verifies storage)
- Search accuracy (validates relevance)

## Performance characteristics

### Embedding generation
- Throughput: ~50-100 texts/second (BGE small, single core)
- Latency: ~10-20ms per text
- Model size: ~50-100MB (cached after first download)

### Vector search
- Index build: O(n log n) for n documents
- Search latency: <10ms for 1000 documents
- Memory: ~100 bytes per vector (384 dims, float32)

### Chunking
- Word chunking: ~1000 docs/second
- Semantic chunking: ~500 docs/second (structure analysis)

## Troubleshooting

### Common issues

**Issue**: "Collection already exists"
- Fix: Use a different collection name or delete the existing one first

**Issue**: "Embedding failed"
- Check: Ensure `local-embeddings` feature is enabled
- Check: Verify model cache at `.fastembed_cache/`

**Issue**: "Search returns no results"
- Check: Verify documents were ingested (`GET /api/rag/collections`)
- Check: Lower the `threshold` parameter
- Check: Ensure query is similar to ingested content

**Issue**: "OOM on reranker"
- Fix: Disable reranking (`"rerank": false`)
- Fix: Use smaller reranker model (`"bge-reranker-small"`)

### Debug mode

Enable verbose logging:
```bash
RUST_LOG=debug ares-server
```

## Future enhancements

- [ ] GPU acceleration for embeddings (ORT execution providers)
- [ ] Multi-tenant vector isolation
- [ ] Incremental document updates
- [ ] Real-time index rebuilding
- [ ] Query expansion and rewriting
- [ ] Few-shot learning integration

## References

- [RAG Handlers](../src/api/handlers/rag.rs)
- [Embedding Service](../src/rag/embeddings.rs)
- [Search Strategies](../src/rag/search.rs)
- [Vector Store](../src/db/ares_vector.rs)
- [Integration Tests](../tests/rag_ehb_ingestion_tests.rs)
