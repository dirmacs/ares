# RAG (Retrieval-Augmented Generation)

The RAG API lets you ingest documents, search them using multiple retrieval strategies, and manage document collections. RAG powers knowledge-grounded responses by retrieving relevant context from your documents before generating answers.

> **Feature flag:** The RAG API requires ARES to be built with the `ares-vector` feature. If your deployment does not include this feature, these endpoints will return `404`.

---

## Ingest documents

```
POST /api/rag/ingest
```

Ingest content into a named collection. The content is automatically chunked and indexed for retrieval.

### Authentication

Requires a JWT access token: `Authorization: Bearer <jwt_access_token>`

### Request body

| Parameter           | Type   | Required | Default  | Description                                                             |
|--------------------|--------|----------|----------|-------------------------------------------------------------------------|
| `collection`        | string | Yes      | --       | Name of the collection to ingest into. Created automatically if it doesn't exist. |
| `content`           | string | Yes      | --       | The text content to ingest.                                             |
| `title`             | string | No       | `null`   | Optional display title for the document.                                |
| `source`            | string | No       | `null`   | Optional source URL or path.                                             |
| `tags`              | array  | No       | `[]`     | Optional tags attached to the document.                                  |
| `chunking_strategy` | string | No       | `null`   | How to split the content. Options include `"word"`, `"semantic"`, and `"character"`. |

### Response

```json
{
  "chunks_created": 5,
  "document_ids": [
    "doc_a1b2c3d4",
    "doc_e5f6g7h8",
    "doc_i9j0k1l2",
    "doc_m3n4o5p6",
    "doc_q7r8s9t0"
  ],
  "collection": "docs"
}
```

| Field           | Type     | Description                                     |
|----------------|----------|-------------------------------------------------|
| `chunks_created` | integer | Number of chunks produced from the content.     |
| `document_ids`   | string[] | IDs assigned to each chunk.                    |
| `collection`     | string   | The collection the content was ingested into.  |

### Examples

#### curl

```bash
curl -X POST http://localhost:3000/api/rag/ingest \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d '{
    "collection": "product-docs",
    "content": "ARES is a multi-agent AI platform that orchestrates specialized agents to handle complex queries. It supports multiple LLM providers including Groq, Anthropic, and NVIDIA...",
    "title": "Product docs overview",
    "source": "docs/product.md",
    "tags": ["documentation"],
    "chunking_strategy": "word"
  }'
```

#### Rust CLI

```bash
ares-server rag ingest-dir \
  --host http://localhost:3000 \
  --token "$ARES_TOKEN" \
  --collection product-docs \
  --docs-path ./docs \
  --chunking-strategy word \
  --tag documentation
```

#### JavaScript

```javascript
const response = await fetch("http://localhost:3000/api/rag/ingest", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
    "Authorization": "Bearer eyJhbGciOi..."
  },
  body: JSON.stringify({
    collection: "product-docs",
    content: "ARES is a multi-agent AI platform...",
    title: "Product docs overview",
    source: "docs/product.md",
    tags: ["documentation"],
    chunking_strategy: "word"
  })
});

const result = await response.json();
console.log(`Created ${result.chunks_created} chunks in '${result.collection}'`);
```

---

## Search documents

```
POST /api/rag/search
```

Search a collection using one of several retrieval strategies. Returns the most relevant document chunks.

### Authentication

Requires a JWT access token: `Authorization: Bearer <jwt_access_token>`

### Request body

| Parameter    | Type    | Required | Default      | Description                                                |
|-------------|---------|----------|--------------|------------------------------------------------------------|
| `collection` | string  | Yes      | --           | Collection to search.                                      |
| `query`      | string  | Yes      | --           | The search query.                                          |
| `strategy`   | string  | No       | `null`       | Retrieval strategy (see below).                            |
| `limit`      | integer | No       | 10           | Maximum number of results to return.                       |
| `rerank`     | boolean | No       | `false`      | Whether to rerank results for improved relevance ordering. |

### Search strategies

| Strategy   | Description                                                                                                  |
|-----------|--------------------------------------------------------------------------------------------------------------|
| `semantic` | Vector similarity search. Best for conceptual or meaning-based queries.                                      |
| `bm25`     | Classic keyword-based ranking (BM25 algorithm). Best for exact term matching.                                |
| `fuzzy`    | Tolerates typos and approximate matches. Useful for user-facing search with imprecise input.                 |
| `hybrid`   | Combines semantic and keyword search, then merges results. Best overall performance for most use cases.      |

### Response

The response contains an array of matching document chunks, each with its content, relevance score, and metadata.

### Examples

#### curl

```bash
curl -X POST http://localhost:3000/api/rag/search \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d '{
    "collection": "product-docs",
    "query": "how does agent routing work",
    "strategy": "hybrid",
    "limit": 5,
    "rerank": true
  }'
```

#### Rust CLI

```bash
ares-server rag search \
  --host http://localhost:3000 \
  --token "$ARES_TOKEN" \
  --collection product-docs \
  --query "how does agent routing work" \
  --strategy hybrid \
  --top-k 5
```

#### JavaScript

```javascript
const response = await fetch("http://localhost:3000/api/rag/search", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
    "Authorization": "Bearer eyJhbGciOi..."
  },
  body: JSON.stringify({
    collection: "product-docs",
    query: "how does agent routing work",
    strategy: "hybrid",
    limit: 5,
    rerank: true
  })
});

const results = await response.json();
results.results.forEach(result => console.log(result));
```

---

## List collections

```
GET /api/rag/collections
```

Returns all document collections for the authenticated user.

### Authentication

Requires a JWT access token: `Authorization: Bearer <jwt_access_token>`

```bash
curl http://localhost:3000/api/rag/collections \
  -H "Authorization: Bearer eyJhbGciOi..."
```

---

## Delete a collection

```
DELETE /api/rag/collection
```

Permanently delete a collection and all its indexed documents.

### Authentication

Requires a JWT access token: `Authorization: Bearer <jwt_access_token>`

### Request body

```json
{
  "collection": "product-docs"
}
```

### Example

```bash
curl -X DELETE http://localhost:3000/api/rag/collection \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d '{"collection": "product-docs"}'
```
