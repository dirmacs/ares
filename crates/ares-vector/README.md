# ares-vector

Pure-Rust embedded vector database with HNSW indexing for ARES.

## Features

- HNSW (Hierarchical Navigable Small World) index for fast approximate nearest neighbor search
- Pure Rust — no external dependencies or services required
- Persistent storage with memory-mapped files
- Collection-based organization
- Cosine similarity search with configurable top-k and threshold

## Usage

```rust
use ares_vector::AresVectorStore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create or open a vector store
    let store = AresVectorStore::new(Some("/tmp/vectors".to_string())).await?;

    // Create a collection
    store.create_collection("docs", 384).await?;

    // Search with a query vector
    let query = vec![0.1f32; 384];
    let results = store.search("docs", &query, 10, 0.5).await?;

    for result in results {
        println!("{}: {:.4}", result.document.id, result.score);
    }

    Ok(())
}
```

## License

MIT
