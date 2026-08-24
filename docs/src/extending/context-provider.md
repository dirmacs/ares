# ContextProvider Trait

ARES provides the `ContextProvider` trait. Extension crates implement it to inject external context into every agent call before the LLM invocation.

## How it works

Before every LLM call, ARES calls `get_context(agent_name, tenant_id)` on the configured provider. If it returns `Some(context)`, ARES prepends the context to the system prompt of the agent.

By default, ARES uses `NoOpContextProvider`, which returns `None`. Agents then run with only their configured system prompt.

## Implementing your own

```rust
use ares_agent::context_provider::ContextProvider;
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

## Registering the provider

Provide a `ContextProviderHandle` on the Cordis `Context`. The `ares_server` facade re-exports `Context`:

```rust
use std::sync::Arc;
use ares_agent::ContextProviderHandle;
use ares_server::Context;

let ctx = Context::new_root();
ctx.provide(ContextProviderHandle::new(Arc::new(MyKnowledgeProvider {
    api_url: "http://localhost:8081".to_string(),
})));
```

## Use cases

- **Knowledge base injection:** fetch relevant docs for each agent and tenant
- **User preference injection:** personalize agent behavior from user history
- **Compliance constraints:** inject regulatory rules into agent prompts
- **RAG augmentation:** supplement the built-in RAG with external retrieval
