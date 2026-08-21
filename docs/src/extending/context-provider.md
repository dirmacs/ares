# ContextProvider Trait

ARES provides a `ContextProvider` trait that lets extension crates inject external context into every agent call before LLM invocation.

## How it works

Before every LLM call, ARES checks `state.context_provider.get_context(agent_name, tenant_id)`. If it returns `Some(context)`, the context is prepended to the agent's system prompt.

By default, ARES uses `NoOpContextProvider` which returns `None`, agents run with their configured system prompt only.

## Implementing your own

```rust
use ares::agents::context_provider::ContextProvider;
use async_trait::async_trait;

struct MyKnowledgeProvider {
    api_url: String,
}

#[async_trait]
impl ContextProvider for MyKnowledgeProvider {
    async fn get_context(
        &self,
        agent_name: &str,
        tenant_id: &str,
    ) -> Option<String> {
        // Fetch relevant context from your knowledge base
        // Return None if no context available
        let url = format!("{}/context/{}/{}", self.api_url, tenant_id, agent_name);
        reqwest::get(&url).await.ok()?.text().await.ok()
    }
}
```

## Wiring into AppState

```rust
use std::sync::Arc;

let state = AppState {
    context_provider: Arc::new(MyKnowledgeProvider {
        api_url: "http://localhost:8081".to_string(),
    }),
    // ... other fields
};
```

## Use cases

- **Knowledge base injection**, fetch relevant docs per agent and tenant
- **User preference injection**, personalize agent behavior based on user history
- **Compliance constraints**, inject regulatory rules into agent prompts
- **RAG augmentation**, supplement the built-in RAG with external retrieval
