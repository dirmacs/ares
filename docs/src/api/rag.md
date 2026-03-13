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
| `metadata`          | object | No       | `{}`     | Arbitrary key-value metadata attached to the document.                  |
| `chunking_strategy` | string | No       | `"word"` | How to split the content into chunks. Options: `"word"`, `"sentence"`, `"paragraph"`. |

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
curl -X POST https://api.ares.dirmacs.com/api/rag/ingest \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d '{
    "collection": "product-docs",
    "content": "ARES is a multi-agent AI platform that orchestrates specialized agents to handle complex queries. It supports multiple LLM providers including Groq, Anthropic, and NVIDIA...",
    "metadata": {
      "source": "documentation",
      "version": "2.0",
      "author": "engineering"
    },
    "chunking_strategy": "paragraph"
  }'
```

#### Python

```python
import requests

response = requests.post(
    "https://api.ares.dirmacs.com/api/rag/ingest",
    headers={
        "Content-Type": "application/json",
        "Authorization": "Bearer eyJhbGciOi..."
    },
    json={
        "collection": "product-docs",
        "content": "ARES is a multi-agent AI platform...",
        "metadata": {"source": "documentation", "version": "2.0"},
        "chunking_strategy": "paragraph"
    }
)

result = response.json()
print(f"Created {result['chunks_created']} chunks in '{result['collection']}'")
```

#### JavaScript

```javascript
const response = await fetch("https://api.ares.dirmacs.com/api/rag/ingest", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
    "Authorization": "Bearer eyJhbGciOi..."
  },
  body: JSON.stringify({
    collection: "product-docs",
    content: "ARES is a multi-agent AI platform...",
    metadata: { source: "documentation", version: "2.0" },
    chunking_strategy: "paragraph"
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
| `strategy`   | string  | No       | `"hybrid"`   | Retrieval strategy (see below).                            |
| `top_k`      | integer | No       | 5            | Maximum number of results to return.                       |
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
curl -X POST https://api.ares.dirmacs.com/api/rag/search \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d '{
    "collection": "product-docs",
    "query": "how does agent routing work",
    "strategy": "hybrid",
    "top_k": 5,
    "rerank": true
  }'
```

#### Python

```python
import requests

response = requests.post(
    "https://api.ares.dirmacs.com/api/rag/search",
    headers={
        "Content-Type": "application/json",
        "Authorization": "Bearer eyJhbGciOi..."
    },
    json={
        "collection": "product-docs",
        "query": "how does agent routing work",
        "strategy": "hybrid",
        "top_k": 5,
        "rerank": True
    }
)

results = response.json()
for result in results:
    print(result)
```

#### JavaScript

```javascript
const response = await fetch("https://api.ares.dirmacs.com/api/rag/search", {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
    "Authorization": "Bearer eyJhbGciOi..."
  },
  body: JSON.stringify({
    collection: "product-docs",
    query: "how does agent routing work",
    strategy: "hybrid",
    top_k: 5,
    rerank: true
  })
});

const results = await response.json();
results.forEach(result => console.log(result));
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
curl https://api.ares.dirmacs.com/api/rag/collections \
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
curl -X DELETE https://api.ares.dirmacs.com/api/rag/collection \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhbGciOi..." \
  -d '{"collection": "product-docs"}'
```
