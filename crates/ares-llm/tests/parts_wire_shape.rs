//! Wiremock-backed proof of the OpenAI-compatible wire shape ARES emits for
//! multimodal `ConversationMessage` parts, independent of any live provider.
//!
//! Construction mirrors `tests/live_nvidia.rs`: a [`ProviderRegistry`] with an
//! `OpenAI`-kind [`ProviderConfig`] whose `api_base` points at a local
//! wiremock server, driven through [`ConfigBasedLLMFactory`] (the loader's
//! own path — never a hand-rolled `genai::Client`). The non-streaming
//! `generate_with_tools_and_history` call is used (shared by the tool loop
//! with `stream_with_tools_and_history`) because it makes a single request
//! against a trivial non-streaming JSON mock body.

use ares_llm::coordinator::ConversationMessage;
use ares_llm::{ConfigBasedLLMFactory, LLMClient, ModelConfig, ProviderConfig, ProviderRegistry};
use ares_types::types::ContentPart as AresContentPart;
use serde_json::Value;
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Env var used to satisfy `require_env` in `genai_from_config`. Distinct
/// from `NVIDIA_API_KEY`/`OPENAI_API_KEY` so this never collides with a real
/// credential; the wiremock server does not check its value.
const KEY_ENV: &str = "ARES_TEST_PARTS_WIRE_SHAPE_KEY";

/// Minimal valid non-streaming OpenAI-compatible chat completion body.
/// `to_chat_response` (genai's OpenAI adapter) only reads
/// `/choices/0/finish_reason` and `/choices/0/message/content`; everything
/// else (usage, id, model) is optional.
fn ok_completion_body() -> Value {
    serde_json::json!({
        "choices": [{
            "finish_reason": "stop",
            "message": { "content": "ok" }
        }]
    })
}

async fn mount_completion(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_completion_body()))
        .mount(server)
        .await;
}

/// Build a client the same way `live_nvidia.rs` does: `ProviderRegistry` +
/// `ConfigBasedLLMFactory`, with the `OpenAI` provider's `api_base` pointed
/// at the wiremock server instead of a real endpoint.
async fn client_against(server: &MockServer) -> Box<dyn LLMClient> {
    // SAFETY: test-only env var, never read concurrently with another test
    // that mutates it (each test uses this same key/value).
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

/// Send `user` through the wiremock server and return the JSON body of the
/// single captured `POST /v1/chat/completions` request.
async fn wire_request_body(user: ConversationMessage) -> Value {
    let server = MockServer::start().await;
    mount_completion(&server).await;
    let client = client_against(&server).await;

    client
        .generate_with_tools_and_history(&[user], &[])
        .await
        .expect("wiremock chat completion call should succeed");

    let requests = server
        .received_requests()
        .await
        .expect("request recording is enabled by default on MockServer::start");
    assert_eq!(
        requests.len(),
        1,
        "expected exactly one POST /v1/chat/completions"
    );
    let req = &requests[0];
    assert_eq!(req.url.path(), "/v1/chat/completions");
    req.body_json::<Value>()
        .expect("request body must be valid JSON")
}

#[tokio::test]
async fn multimodal_parts_wire_shape() {
    let user = ConversationMessage {
        parts: vec![
            AresContentPart::ImageBase64 {
                mime: "image/png".to_string(),
                data: "AAAA".to_string(),
            },
            AresContentPart::FileBase64 {
                mime: "application/pdf".to_string(),
                data: "BBBB".to_string(),
                name: Some("doc.pdf".to_string()),
            },
            AresContentPart::ImageUrl {
                url: "https://x/y.png".to_string(),
            },
        ],
        ..ConversationMessage::user("describe")
    };

    let body = wire_request_body(user).await;

    let content = body["messages"][0]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("messages[0].content must be an array: {body:#?}"));

    // 0.11.3 fallback fix: the typed `content` prompt must be prepended as a
    // `text` part (parts carry no Text part of their own here), not dropped.
    assert_eq!(
        content[0],
        serde_json::json!({"type": "text", "text": "describe"}),
        "first content element must be the typed prompt as a text part: {content:#?}"
    );

    let has_image_data_url = content.iter().any(|p| {
        p["type"] == "image_url"
            && p["image_url"]["url"]
                .as_str()
                .is_some_and(|u| u.starts_with("data:image/png;base64,AAAA"))
    });
    assert!(
        has_image_data_url,
        "expected an image_url element with a data: URL for the base64 PNG part: {content:#?}"
    );

    let has_plain_image_url = content
        .iter()
        .any(|p| p["type"] == "image_url" && p["image_url"]["url"] == "https://x/y.png");
    assert!(
        has_plain_image_url,
        "expected an image_url element with url == https://x/y.png for the ImageUrl part: {content:#?}"
    );

    // genai 0.7.0-beta.19's OpenAI adapter (adapter_shared.rs ~line 385-391):
    // a non-image/audio/video Binary from a base64 source becomes
    // `{"type":"file","file":{"filename":<name>,"file_data":"data:<mime>;base64,<data>"}}`
    // (there is no `image_url` fallback for PDFs — only URL-sourced non-image
    // binaries fall back, and they are dropped with a warning, not the base64
    // ones used here).
    let file_part = content
        .iter()
        .find(|p| p["type"] == "file")
        .unwrap_or_else(|| panic!("expected a file element for the base64 PDF part: {content:#?}"));
    assert_eq!(file_part["file"]["filename"], "doc.pdf");
    assert_eq!(
        file_part["file"]["file_data"],
        "data:application/pdf;base64,BBBB"
    );

    assert_eq!(
        content.len(),
        4,
        "expected exactly 4 content parts (text + 3 attachments): {content:#?}"
    );
}

#[tokio::test]
async fn text_part_matching_content_is_not_duplicated() {
    let user = ConversationMessage {
        parts: vec![AresContentPart::Text {
            text: "describe".to_string(),
        }],
        ..ConversationMessage::user("describe")
    };

    let body = wire_request_body(user).await;
    let content = &body["messages"][0]["content"];

    // A single Text part equal to `content` is text-only, so genai collapses
    // it to a plain string rather than a one-element content-part array —
    // either shape is a pass here as long as there is exactly one "describe".
    let text_count = match content {
        Value::String(s) => {
            assert_eq!(s, "describe");
            1
        }
        Value::Array(parts) => {
            for part in parts {
                assert_eq!(part["type"], "text", "unexpected non-text part: {parts:#?}");
            }
            parts
                .iter()
                .filter(|p| p["type"] == "text" && p["text"] == "describe")
                .count()
        }
        other => panic!("unexpected messages[0].content shape: {other:?}"),
    };
    assert_eq!(
        text_count, 1,
        "exactly one text element equal to \"describe\", no duplication: {content:#?}"
    );
}
