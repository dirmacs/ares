//! Wiremock proof that a streamed completion can surface the provider's own
//! token usage, and that the pre-existing text-only stream path is untouched.
//!
//! Fixtures are real SSE bodies in the shape OpenAI sends when
//! `stream_options.include_usage` is set (the client already requests it):
//! content deltas, a `finish_reason` chunk, then a trailing `choices: []` chunk
//! carrying `usage`, then `[DONE]`.
//!
//! The no-usage fixture is the same stream with the usage chunk omitted, which
//! is what a provider sends when it declines to report counts. That case must
//! produce NO `Usage` item — never a zero that a caller could mistake for a
//! real, billable count.

use ares_llm::{
    ConfigBasedLLMFactory, LLMClient, LlmStreamItem, ModelConfig, ProviderConfig, ProviderRegistry,
    TokenUsage,
};
use ares_types::types::{AppError, Result, ToolDefinition};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Distinct from the other test files' env var so this never collides with a
/// real credential; the wiremock server does not check its value.
const KEY_ENV: &str = "ARES_TEST_STREAM_USAGE_KEY";

/// SSE body whose trailing `choices: []` chunk carries provider counts.
fn sse_with_usage() -> String {
    [
        r#"data: {"choices":[{"index":0,"delta":{"content":"Hello"}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{"content":" world"}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "\n\n",
        r#"data: {"choices":[],"usage":{"prompt_tokens":1234,"completion_tokens":567,"total_tokens":1801,"prompt_tokens_details":{"cached_tokens":64}}}"#,
        "\n\n",
        "data: [DONE]",
        "\n\n",
    ]
    .concat()
}

/// Same stream with no usage chunk anywhere.
fn sse_without_usage() -> String {
    [
        r#"data: {"choices":[{"index":0,"delta":{"content":"Hello"}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{"content":" world"}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "\n\n",
        "data: [DONE]",
        "\n\n",
    ]
    .concat()
}

async fn mount_sse(server: &MockServer, body: String) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "text/event-stream")
                .set_body_raw(body, "text/event-stream"),
        )
        .mount(server)
        .await;
}

/// Build a client against the wiremock server the same way `parts_wire_shape.rs`
/// does: `ProviderRegistry` + `ConfigBasedLLMFactory`, `OpenAI` kind with
/// `api_base` pointed at the local server (never a hand-rolled `genai::Client`).
async fn client_against(server: &MockServer) -> Box<dyn LLMClient> {
    // SAFETY: test-only env var, never read concurrently with another test that
    // mutates a different one (each test uses this same key/value).
    unsafe {
        std::env::set_var(KEY_ENV, "wiremock-test-key");
    }
    let mut registry = ProviderRegistry::new();
    registry.register_provider(
        "wiremock",
        ProviderConfig::OpenAI {
            api_key_env: KEY_ENV.to_string(),
            api_base: format!("{}/v1", server.uri()),
            default_model: "test-model".to_string(),
        },
    );
    registry.register_model(
        "wiremock-chat",
        ModelConfig {
            provider: "wiremock".to_string(),
            model: "test-model".to_string(),
            temperature: 0.2,
            max_tokens: 64,
        },
    );
    let factory = ConfigBasedLLMFactory::new(Arc::new(registry), "wiremock-chat");
    factory
        .create_default()
        .await
        .expect("factory should build a client against the wiremock server")
}

async fn collect_usage_items(client: &dyn LLMClient) -> Vec<LlmStreamItem> {
    let mut stream = client
        .stream_with_history_and_usage(&[("user".into(), "hi".to_string())])
        .await
        .expect("usage stream should open against the wiremock server");
    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item.expect("stream item should be Ok"));
    }
    items
}

#[tokio::test]
async fn stream_reports_provider_reported_usage() {
    let server = MockServer::start().await;
    mount_sse(&server, sse_with_usage()).await;
    let client = client_against(&server).await;

    let items = collect_usage_items(client.as_ref()).await;

    let usage: Vec<&TokenUsage> = items
        .iter()
        .filter_map(|i| match i {
            LlmStreamItem::Usage(u) => Some(u),
            LlmStreamItem::Text(_) => None,
        })
        .collect();
    assert_eq!(
        usage.len(),
        1,
        "expected exactly one terminal Usage item, got {items:?}"
    );
    assert_eq!(
        usage[0],
        &TokenUsage {
            prompt_tokens: 1234,
            completion_tokens: 567,
            total_tokens: 1801,
            cached_tokens: Some(64),
        },
        "the reported count must be the provider's, not a local estimate"
    );

    // Text still streams, unchanged, ahead of the usage terminal.
    let text: String = items
        .iter()
        .filter_map(|i| match i {
            LlmStreamItem::Text(t) => Some(t.as_str()),
            LlmStreamItem::Usage(_) => None,
        })
        .collect();
    assert_eq!(text, "Hello world", "text chunks must be unaffected");
    assert!(
        matches!(items.last(), Some(LlmStreamItem::Usage(_))),
        "usage must be the terminal item"
    );
}

#[tokio::test]
async fn stream_without_usage_frame_yields_no_usage_item() {
    let server = MockServer::start().await;
    mount_sse(&server, sse_without_usage()).await;
    let client = client_against(&server).await;

    let items = collect_usage_items(client.as_ref()).await;

    assert!(
        !items.iter().any(|i| matches!(i, LlmStreamItem::Usage(_))),
        "a provider that sent no usage frame must yield NO Usage item, not a \
         zero count that looks billable; got {items:?}"
    );
    let text: String = items
        .iter()
        .filter_map(|i| match i {
            LlmStreamItem::Text(t) => Some(t.as_str()),
            LlmStreamItem::Usage(_) => None,
        })
        .collect();
    assert_eq!(text, "Hello world", "text must still stream in full");
}

#[tokio::test]
async fn text_only_stream_path_is_unchanged() {
    let server = MockServer::start().await;
    // The usage-bearing fixture: the pre-existing path must ignore the usage
    // chunk exactly as before, yielding text and nothing else.
    mount_sse(&server, sse_with_usage()).await;
    let client = client_against(&server).await;

    let mut stream = client
        .stream_with_history(&[("user".into(), "hi".to_string())])
        .await
        .expect("text stream should open against the wiremock server");
    let mut chunks = Vec::new();
    while let Some(item) = stream.next().await {
        chunks.push(item.expect("stream item should be Ok"));
    }

    assert_eq!(
        chunks,
        vec!["Hello".to_string(), " world".to_string()],
        "existing callers of stream_with_history must see the same text chunks"
    );
}

/// A downstream implementor that knows nothing about stream usage. Its
/// existence here is the compatibility argument: it compiles against the new
/// trait method without declaring it, and the inherited default streams text
/// while honestly reporting that no usage is available.
struct TextOnlyClient;

#[async_trait]
impl LLMClient for TextOnlyClient {
    async fn generate(&self, _prompt: &str) -> Result<String> {
        Ok(String::new())
    }

    async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
        Ok(String::new())
    }

    async fn generate_with_history(
        &self,
        _messages: &[(String, String)],
    ) -> Result<ares_llm::LLMResponse> {
        Err(AppError::FeatureDisabled("no".into()))
    }

    async fn generate_with_tools(
        &self,
        _prompt: &str,
        _tools: &[ToolDefinition],
    ) -> Result<ares_llm::LLMResponse> {
        Err(AppError::FeatureDisabled("no".into()))
    }

    async fn generate_with_tools_and_history(
        &self,
        _messages: &[ares_llm::ConversationMessage],
        _tools: &[ToolDefinition],
    ) -> Result<ares_llm::LLMResponse> {
        Err(AppError::FeatureDisabled("no".into()))
    }

    async fn stream(
        &self,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        Err(AppError::FeatureDisabled("no".into()))
    }

    async fn stream_with_system(
        &self,
        _system: &str,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        Err(AppError::FeatureDisabled("no".into()))
    }

    async fn stream_with_history(
        &self,
        _messages: &[(String, String)],
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let s = async_stream::stream! {
            yield Ok("Hello".to_string());
            yield Ok(" world".to_string());
        };
        Ok(Box::new(Box::pin(s)))
    }

    fn model_name(&self) -> &str {
        "text-only"
    }
}

#[tokio::test]
async fn default_impl_streams_text_without_inventing_usage() {
    let mut stream = TextOnlyClient
        .stream_with_history_and_usage(&[("user".into(), "hi".to_string())])
        .await
        .expect("default impl delegates to stream_with_history");
    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item.expect("stream item should be Ok"));
    }

    assert_eq!(
        items,
        vec![
            LlmStreamItem::Text("Hello".to_string()),
            LlmStreamItem::Text(" world".to_string()),
        ],
        "an implementor that declares nothing must get text and no usage"
    );
}

// Guards the fixture's own shape: the request must actually ask for usage, or
// the "with usage" test would pass for the wrong reason.
#[tokio::test]
async fn client_requests_stream_usage() {
    let server = MockServer::start().await;
    mount_sse(&server, sse_without_usage()).await;
    let client = client_against(&server).await;
    let _ = collect_usage_items(client.as_ref()).await;

    let requests = server
        .received_requests()
        .await
        .expect("request recording is enabled by default on MockServer::start");
    assert_eq!(requests.len(), 1, "expected one streamed POST");
    let body: Value = requests[0]
        .body_json::<Value>()
        .expect("request body must be valid JSON");
    assert_eq!(
        body["stream_options"]["include_usage"],
        Value::Bool(true),
        "the client must ask the provider for usage; body: {body:#?}"
    );
    assert_eq!(body["stream"], Value::Bool(true));
}
