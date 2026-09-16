//! Live Anthropic integration through the ARES LLM stack.
//!
//! Mirrors `live_nvidia.rs` but routes through the native Anthropic adapter
//! (genai `AdapterKind::Anthropic`) instead of the NVIDIA proxy path.
//! Opt-in: requires `ANTHROPIC_API_KEY`. The model id comes from
//! `ANTHROPIC_CHAT_MODEL` or defaults to a cheap Haiku. Admin-style keys
//! that are not workspace-scoped additionally need `ANTHROPIC_WORKSPACE_ID`,
//! which travels as the `anthropic-workspace-id` header.

use ares_llm::provider_registry::RuntimeProviderEntry;
use ares_llm::{ConfigBasedLLMFactory, LLMClient, ModelConfig, ProviderRegistry};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const KEY_ENV: &str = "ANTHROPIC_API_KEY";
const WS_ENV: &str = "ANTHROPIC_WORKSPACE_ID";
const CALL_TIMEOUT: Duration = Duration::from_secs(90);

fn chat_model() -> String {
    match std::env::var("ANTHROPIC_CHAT_MODEL") {
        Ok(m) if !m.trim().is_empty() => m.trim().to_string(),
        _ => "claude-haiku-4-5-20251001".to_string(),
    }
}

fn redact(msg: &str) -> String {
    let mut out = msg.to_string();
    for env in [KEY_ENV, WS_ENV] {
        if let Ok(secret) = std::env::var(env) {
            if !secret.is_empty() {
                out = out.replace(&secret, "[REDACTED]");
            }
        }
    }
    out
}

async fn live_client() -> Box<dyn LLMClient> {
    let model = chat_model();
    let key = std::env::var(KEY_ENV).unwrap_or_default();
    let mut headers = HashMap::new();
    if let Ok(ws) = std::env::var(WS_ENV) {
        let ws = ws.trim().to_string();
        if !ws.is_empty() {
            headers.insert("anthropic-workspace-id".to_string(), ws);
        }
    }
    let entry = RuntimeProviderEntry {
        tenant_id: None,
        display_name: "live-anthropic".to_string(),
        provider_type: "anthropic-compatible".to_string(),
        api_base: String::new(),
        auth_type: "api_key".to_string(),
        default_model: Some(model.clone()),
        headers,
        api_key: Some(key),
        enabled: true,
    };
    let mut registry = ProviderRegistry::from_config(HashMap::new(), HashMap::new(), None);
    registry.reload_runtime_providers(vec![entry], vec!["live-anthropic".to_string()]);
    registry.register_model(
        "live-chat",
        ModelConfig {
            provider: "live-anthropic".to_string(),
            model: model.clone(),
            temperature: 0.2,
            max_tokens: 64,
        },
    );
    let factory = ConfigBasedLLMFactory::new(Arc::new(registry), "live-chat");
    eprintln!("anthropic chat model: {model}");
    match factory.create_default().await {
        Ok(c) => c,
        Err(e) => panic!("live anthropic client: {}", redact(&e.to_string())),
    }
}

#[tokio::test]
#[ignore]
async fn live_anthropic_complete() {
    if std::env::var(KEY_ENV)
        .map(|k| k.trim().is_empty())
        .unwrap_or(true)
    {
        eprintln!("SKIPPED live_anthropic_complete: {KEY_ENV} unset");
        return;
    }
    let client = live_client().await;
    let text = tokio::time::timeout(
        CALL_TIMEOUT,
        client.generate("Reply with exactly one word: pong"),
    )
    .await
    .unwrap_or_else(|_| panic!("live_anthropic_complete timed out"))
    .unwrap_or_else(|e| {
        panic!(
            "live_anthropic_complete generate: {}",
            redact(&e.to_string())
        )
    });
    assert!(
        text.to_lowercase().contains("pong"),
        "unexpected reply: {}",
        redact(&text)
    );
}
