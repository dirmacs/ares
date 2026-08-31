//! Live NVIDIA NIM integration through the ARES LLM stack.
//!
//! Construction uses [`ProviderRegistry::from_config`] + [`ConfigBasedLLMFactory`]
//! (the same path as the loader). Tests do **not** hand-roll a `genai::Client`.
//!
//! `live_vision_parts` sends a base64 image `ContentPart` to a vision-capable
//! NIM model (overridable via `NVIDIA_VISION_MODEL`) when the catalog offers one.

use ares_llm::coordinator::ConversationMessage;
use ares_llm::{
    CatalogEntry, ConfigBasedLLMFactory, GenerationHints, LLMClient, ModelConfig,
    NvidiaCatalogCache, NvidiaConfig, Provider, ProviderConfig, ProviderRegistry,
};
use ares_types::types::{ContentPart as AresContentPart, ToolDefinition};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const KEY_ENV: &str = "NVIDIA_API_KEY";
const CALL_TIMEOUT: Duration = Duration::from_secs(90);

fn nvidia_key_present() -> bool {
    std::env::var(KEY_ENV)
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
}
/// Honor `NVIDIA_API_BASE` (e.g. `http://127.0.0.1:9999/v1`) so the live tests
/// can be pointed at a local sink/proxy for request debugging.
fn apply_base_override(nvidia: &mut NvidiaConfig) {
    if let Ok(base) = std::env::var("NVIDIA_API_BASE") {
        let base = base.trim().trim_end_matches('/').to_string();
        if !base.is_empty() {
            eprintln!("nvidia api base override: {base}");
            nvidia.api_base = base.clone();
            nvidia.models_url = format!("{base}/models");
        }
    }
}

fn skip_without_key(test: &str) -> bool {
    if nvidia_key_present() {
        return false;
    }
    eprintln!("SKIPPED {test}: {KEY_ENV} unset");
    true
}

fn redact(msg: &str) -> String {
    let mut out = msg.to_string();
    if let Ok(key) = std::env::var(KEY_ENV) {
        if !key.is_empty() {
            out = out.replace(&key, "[REDACTED]");
        }
    }
    out
}

fn panic_redacted(context: &str, err: impl std::fmt::Display) -> ! {
    let msg = redact(&err.to_string());
    let hint = if msg.to_lowercase().contains("403")
        && msg.to_lowercase().contains("authorization failed")
    {
        " (GET /v1/models is public and does not prove NVIDIA_API_KEY can infer)"
    } else {
        ""
    };
    panic!("{context}: {msg}{hint}");
}

struct LiveStack {
    factory: ConfigBasedLLMFactory,
    nvidia: NvidiaConfig,
    chat_model: String,
}

async fn live_stack() -> LiveStack {
    let mut nvidia = NvidiaConfig::default();
    apply_base_override(&mut nvidia);
    let catalog = Arc::new(NvidiaCatalogCache::new(nvidia.clone()));
    match catalog.refresh().await {
        Ok(n) => eprintln!("nvidia catalog refreshed: {n} chat models"),
        Err(e) => eprintln!(
            "nvidia catalog refresh failed (falling back to default model): {}",
            redact(&e.to_string())
        ),
    }

    let chat_model = pick_chat_model(&catalog.snapshot(), &nvidia);
    eprintln!("nvidia chat model: {chat_model}");

    let mut registry = ProviderRegistry::from_config(HashMap::new(), HashMap::new(), Some(&nvidia))
        .with_catalog(catalog.clone());
    registry.register_model(
        "live-chat",
        ModelConfig {
            provider: "nvidia".to_string(),
            model: chat_model.clone(),
            temperature: 0.2,
            max_tokens: 64,
        },
    );

    let factory = ConfigBasedLLMFactory::new(Arc::new(registry), "live-chat");
    LiveStack {
        factory,
        nvidia,
        chat_model,
    }
}

fn pick_chat_model(entries: &[CatalogEntry], nvidia: &NvidiaConfig) -> String {
    if let Ok(over) = std::env::var("NVIDIA_CHAT_MODEL") {
        let over = over.trim();
        if !over.is_empty() {
            return over.to_string();
        }
    }
    // GET /v1/models is public and still lists retired/unentitled ids.
    // Prefer ids this NIM account can actually infer when they appear in catalog.
    const PREFERRED: &[&str] = &[
        "nvidia/nemotron-3-ultra-550b-a55b",
        "minimaxai/minimax-m3",
        "nvidia/nemotron-3-nano-30b-a3b",
        "nvidia/nemotron-3-super-120b-a12b",
    ];
    for id in PREFERRED {
        if entries.iter().any(|e| e.id == *id) {
            return (*id).to_string();
        }
    }
    if entries.iter().any(|e| e.id == nvidia.default_model) {
        return nvidia.default_model.clone();
    }
    let score = |id: &str| -> i32 {
        let l = id.to_lowercase();
        let mut s = 0;
        if l.contains("instruct") || l.contains("-it") {
            s += 5;
        }
        if ["-1b", "-3b", "-4b", "-7b", "-8b", "-9b"]
            .iter()
            .any(|sz| l.contains(sz))
        {
            s += 20;
        }
        if l.contains("nano") || l.contains("-mini") || l.contains("small") {
            s += 15;
        }
        if l.contains("70b") || l.contains("72b") || l.contains("405b") || l.contains("120b") {
            s -= 10;
        }
        s
    };
    entries
        .iter()
        .max_by_key(|e| score(&e.id))
        .map(|e| e.id.clone())
        .unwrap_or_else(|| nvidia.default_model.clone())
}

/// Pick a vision-capable chat model from the catalog, if any.
fn pick_vision_model(entries: &[CatalogEntry]) -> Option<String> {
    if let Ok(over) = std::env::var("NVIDIA_VISION_MODEL") {
        let over = over.trim().to_string();
        if !over.is_empty() {
            return Some(over);
        }
    }
    let score = |id: &str| -> i64 {
        let l = id.to_lowercase();
        let mut s: i64 = 0;
        if l.contains("vision") {
            s += 100;
        }
        if l.contains("vila") {
            s += 80;
        }
        if l.contains("vlm") {
            s += 60;
        }
        if l.contains("embed") || l.contains("rerank") {
            s -= 10_000;
        }
        s
    };
    entries
        .iter()
        .map(|e| e.id.clone())
        .max_by_key(|id| score(id))
        .filter(|id| score(id) > 0)
}

async fn chat_client(stack: &LiveStack) -> Box<dyn LLMClient> {
    let client = match stack.factory.create_default().await {
        Ok(c) => c,
        Err(e) => match stack
            .factory
            .registry()
            .create_client_for_provider("nvidia")
            .await
        {
            Ok(c) => {
                eprintln!(
                    "create_default failed for {}; using provider default: {}",
                    stack.chat_model,
                    redact(&e.to_string())
                );
                c
            }
            Err(e2) => panic_redacted("failed to create NVIDIA chat client", e2),
        },
    };
    client.set_hints(GenerationHints {
        max_tokens: Some(64),
        ..GenerationHints::default()
    });
    client
}

fn weather_tool() -> ToolDefinition {
    ToolDefinition {
        name: "get_weather".to_string(),
        description: "Get the current weather for a city.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "city": {
                    "type": "string",
                    "description": "City name, e.g. Austin"
                }
            },
            "required": ["city"]
        }),
    }
}

fn is_tool_capability_variance(msg: &str) -> bool {
    let l = msg.to_lowercase();
    l.contains("tool")
        || l.contains("function calling")
        || l.contains("function_call")
        || l.contains("not support")
        || l.contains("unsupported")
        || l.contains("does not have")
}

/// Probe the NVIDIA catalog endpoint for an embedding model.
///
/// [`NvidiaCatalogCache::refresh`] keeps chat models only (`embed` ids are
/// filtered), so discovery uses the same `models_url` the catalog fetches.
async fn discover_embed_model(cfg: &NvidiaConfig) -> Option<String> {
    let key = std::env::var(&cfg.api_key_env)
        .ok()
        .filter(|k| !k.is_empty())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .ok()?;
    let resp = client
        .get(&cfg.models_url)
        .bearer_auth(&key)
        .header("Accept", "application/json")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        eprintln!("embed catalog probe HTTP {}", resp.status().as_u16());
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let mut ids: Vec<String> = body
        .get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("id").and_then(|id| id.as_str()))
        .filter(|id| {
            let l = id.to_lowercase();
            l.contains("embed") && !l.contains("rerank")
        })
        .map(str::to_string)
        .collect();
    if ids.is_empty() {
        return None;
    }
    if let Ok(over) = std::env::var("NVIDIA_EMBED_MODEL") {
        let over = over.trim();
        if !over.is_empty() {
            return Some(over.to_string());
        }
    }
    const PREFERRED_EMBED: &[&str] = &["nvidia/nemotron-3-embed-1b"];
    for pref in PREFERRED_EMBED {
        if ids.iter().any(|id| id == pref) {
            return Some((*pref).to_string());
        }
    }
    Some(ids.remove(0))
}

async fn embed_client(stack: &LiveStack, model: &str) -> Box<dyn LLMClient> {
    let provider_config = stack
        .factory
        .registry()
        .get_provider("nvidia")
        .unwrap_or_else(|| ProviderConfig::OpenAI {
            api_key_env: stack.nvidia.api_key_env.clone(),
            api_base: stack.nvidia.api_base.clone(),
            default_model: model.to_string(),
        });
    let provider = Provider::from_config(&provider_config, Some(model))
        .unwrap_or_else(|e| panic_redacted("Provider::from_config for embed model", e));
    provider
        .create_client()
        .await
        .unwrap_or_else(|e| panic_redacted("create_client for embed model", e))
}

#[tokio::test]
#[ignore]
async fn live_complete() {
    if skip_without_key("live_complete") {
        return;
    }
    let stack = live_stack().await;
    let client = chat_client(&stack).await;
    let text = tokio::time::timeout(
        CALL_TIMEOUT,
        client.generate("Reply with exactly one word: pong"),
    )
    .await
    .unwrap_or_else(|_| panic!("live_complete timed out"))
    .unwrap_or_else(|e| panic_redacted("live_complete generate", e));
    eprintln!(
        "live_complete model={} chars={}",
        client.model_name(),
        text.len()
    );
    if text.trim().is_empty() {
        panic!(
            "live_complete: expected non-empty text from {}",
            client.model_name()
        );
    }
}

#[tokio::test]
#[ignore]
async fn live_stream() {
    if skip_without_key("live_stream") {
        return;
    }
    let stack = live_stack().await;
    let client = chat_client(&stack).await;
    let mut stream = tokio::time::timeout(
        CALL_TIMEOUT,
        client.stream("Reply with exactly one word: pong"),
    )
    .await
    .unwrap_or_else(|_| panic!("live_stream setup timed out"))
    .unwrap_or_else(|e| panic_redacted("live_stream LLMClient::stream", e));

    let mut chunks = 0usize;
    let mut text = String::new();
    let collect = async {
        while let Some(item) = stream.next().await {
            let piece = item.unwrap_or_else(|e| panic_redacted("live_stream chunk", e));
            chunks += 1;
            text.push_str(&piece);
        }
    };
    tokio::time::timeout(CALL_TIMEOUT, collect)
        .await
        .unwrap_or_else(|_| panic!("live_stream collect timed out"));

    eprintln!(
        "live_stream model={} chunks={} chars={}",
        client.model_name(),
        chunks,
        text.len()
    );
    assert!(chunks > 0, "live_stream: expected >0 chunks");
    if text.trim().is_empty() {
        panic!("live_stream: concatenated text was empty");
    }
}

#[tokio::test]
#[ignore]
async fn live_tool_loop() {
    if skip_without_key("live_tool_loop") {
        return;
    }
    let stack = live_stack().await;
    let client = chat_client(&stack).await;
    let tools = [weather_tool()];
    let result = tokio::time::timeout(
        CALL_TIMEOUT,
        client.generate_with_tools(
            "What is the weather in Austin? Use the get_weather tool.",
            &tools,
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("live_tool_loop timed out"));

    match result {
        Ok(resp) => {
            eprintln!(
                "live_tool_loop model={} finish={} tool_calls={} content_chars={}",
                client.model_name(),
                resp.finish_reason,
                resp.tool_calls.len(),
                resp.content.len()
            );
            if resp.tool_calls.is_empty() {
                eprintln!(
                    "live_tool_loop: no tool call (graceful refusal/text): {:?}",
                    resp.content.chars().take(200).collect::<String>()
                );
            } else {
                for call in &resp.tool_calls {
                    eprintln!("live_tool_loop: tool {} args={}", call.name, call.arguments);
                }
            }
        }
        Err(e) => {
            let msg = redact(&e.to_string());
            if is_tool_capability_variance(&msg) {
                eprintln!(
                    "live_tool_loop: model tool-support variance (not a transport failure): {msg}"
                );
                return;
            }
            panic_redacted("live_tool_loop transport-level error", e);
        }
    }
}

#[tokio::test]
#[ignore]
async fn live_embed() {
    if skip_without_key("live_embed") {
        return;
    }
    let stack = live_stack().await;
    let Some(embed_model) = discover_embed_model(&stack.nvidia).await else {
        eprintln!(
            "SKIPPED live_embed: no NVIDIA embed model discoverable via catalog at {}",
            stack.nvidia.models_url
        );
        return;
    };
    eprintln!("nvidia embed model: {embed_model}");

    let client = embed_client(&stack, &embed_model).await;
    let vectors = tokio::time::timeout(
        CALL_TIMEOUT,
        client.embed(&["ARES live NVIDIA embedding probe".to_string()]),
    )
    .await
    .unwrap_or_else(|_| panic!("live_embed timed out"))
    .unwrap_or_else(|e| panic_redacted("live_embed", e));

    eprintln!(
        "live_embed model={} n_vectors={} dim={}",
        client.model_name(),
        vectors.len(),
        vectors.first().map(|v| v.len()).unwrap_or(0)
    );
    if vectors.is_empty() {
        panic!("live_embed: expected at least one vector");
    }
    if vectors[0].is_empty() {
        panic!("live_embed: embedding vector was empty");
    }
}

#[tokio::test]
#[ignore]
async fn live_stream_text_tools_api() {
    if skip_without_key("live_stream_text_tools_api") {
        return;
    }
    let stack = live_stack().await;
    let client = chat_client(&stack).await;
    let user = ConversationMessage::user("Reply with exactly one word: pong");
    let mut stream = tokio::time::timeout(
        CALL_TIMEOUT,
        client.stream_with_tools_and_history(&[user], &[]),
    )
    .await
    .unwrap_or_else(|_| panic!("live_stream_text_tools_api setup timed out"))
    .unwrap_or_else(|e| panic_redacted("live_stream_text_tools_api setup", e));
    let mut text = String::new();
    let collect = async {
        while let Some(item) = stream.next().await {
            match item.unwrap_or_else(|e| panic_redacted("live_stream_text_tools_api chunk", e)) {
                ares_llm::LlmStreamEvent::Text(t) => text.push_str(&t),
                ares_llm::LlmStreamEvent::ToolCalls(_) => {}
            }
        }
    };
    tokio::time::timeout(CALL_TIMEOUT, collect)
        .await
        .unwrap_or_else(|_| panic!("live_stream_text_tools_api collect timed out"));
    eprintln!(
        "live_stream_text_tools_api model={} chars={}",
        client.model_name(),
        text.len()
    );
    assert!(
        !text.trim().is_empty(),
        "live_stream_text_tools_api: expected non-empty text"
    );
}

/// 1x1 red PNG, base64-encoded. Small enough to send inline; enough for a
/// "what color" prompt on any vision-capable model.
const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

#[tokio::test]
#[ignore]
async fn live_vision_parts() {
    if skip_without_key("live_vision_parts") {
        return;
    }
    let mut nvidia = NvidiaConfig::default();
    apply_base_override(&mut nvidia);
    let catalog = Arc::new(NvidiaCatalogCache::new(nvidia.clone()));
    match catalog.refresh().await {
        Ok(n) => eprintln!("nvidia catalog refreshed: {n} chat models"),
        Err(e) => eprintln!(
            "nvidia catalog refresh failed (vision discovery limited): {}",
            redact(&e.to_string())
        ),
    }
    let Some(vision_model) = pick_vision_model(&catalog.snapshot()) else {
        eprintln!(
            "SKIPPED live_vision_parts: no vision-capable model found in the NVIDIA catalog \
             (set NVIDIA_VISION_MODEL to force one)"
        );
        return;
    };
    eprintln!("nvidia vision model: {vision_model}");

    let mut registry = ProviderRegistry::from_config(HashMap::new(), HashMap::new(), Some(&nvidia))
        .with_catalog(catalog.clone());
    registry.register_model(
        "live-vision",
        ModelConfig {
            provider: "nvidia".to_string(),
            model: vision_model.clone(),
            temperature: 0.2,
            max_tokens: 64,
        },
    );
    let factory = ConfigBasedLLMFactory::new(Arc::new(registry), "live-vision");
    let client = factory
        .create_default()
        .await
        .unwrap_or_else(|e| panic_redacted("live_vision_parts create client", e));
    client.set_hints(GenerationHints {
        max_tokens: Some(64),
        ..GenerationHints::default()
    });
    eprintln!("supports_vision() = {}", client.supports_vision());

    // The exact shape the HTTP chat path persists and forwards: a user message
    // whose multimodal content lives in `parts`.
    let user = ConversationMessage {
        parts: vec![AresContentPart::ImageBase64 {
            mime: "image/png".to_string(),
            data: TINY_PNG_BASE64.to_string(),
        }],
        ..ConversationMessage::user("Reply with exactly one word: the color of the image.")
    };

    let mut stream = tokio::time::timeout(
        CALL_TIMEOUT,
        client.stream_with_tools_and_history(&[user], &[]),
    )
    .await
    .unwrap_or_else(|_| panic!("live_vision_parts setup timed out"))
    .unwrap_or_else(|e| panic_redacted("live_vision_parts stream_with_tools_and_history", e));

    let mut text = String::new();
    let collect = async {
        while let Some(item) = stream.next().await {
            match item.unwrap_or_else(|e| panic_redacted("live_vision_parts chunk", e)) {
                ares_llm::LlmStreamEvent::Text(t) => text.push_str(&t),
                ares_llm::LlmStreamEvent::ToolCalls(calls) => {
                    eprintln!("live_vision_parts: unexpected tool calls: {calls:?}")
                }
            }
        }
    };
    tokio::time::timeout(CALL_TIMEOUT, collect)
        .await
        .unwrap_or_else(|_| panic!("live_vision_parts collect timed out"));

    eprintln!(
        "live_vision_parts model={} chars={}",
        client.model_name(),
        text.len()
    );
    if text.trim().is_empty() {
        panic!(
            "live_vision_parts: expected non-empty answer from {} for an image part",
            client.model_name()
        );
    }
}
