//! genai-backed [`LLMClient`](crate::client::LLMClient).
//!
//! Every chat/embed call is dispatched with an explicit [`ServiceTarget`]
//! (never a bare `&str` model name, which genai would silently treat as Ollama).

use crate::client::{
    CacheControl, GenaiProvider, GenerationHints, LLMClient, LLMResponse, LlmStreamEvent,
    TokenUsage,
};
use crate::coordinator::{ConversationMessage, MessageRole};
use ares_types::types::{AppError, ContentPart as AresPart, Result, ToolCall, ToolDefinition};
use async_trait::async_trait;
use futures::StreamExt;
use genai::adapter::AdapterKind;
use genai::chat::{
    Binary, CacheControl as GenaiCache, ChatMessage, ChatOptions, ChatRequest, ChatResponse,
    ChatResponseFormat, ChatStreamEvent, ContentPart, JsonSpec, MessageContent, MessageOptions,
    ReasoningEffort, Tool, ToolResponse, Usage,
};
use genai::resolver::{AuthData, Endpoint};
use genai::{Client, ModelIden, ServiceTarget};
use std::sync::RwLock;
use std::time::Duration;

const PROVIDER_WEB_SEARCH: &str = "provider_web_search";

/// HTTP LLM client over `genai` 0.7.
pub struct GenaiClient {
    inner: Client,
    provider: GenaiProvider,
    hints: RwLock<GenerationHints>,
}

impl GenaiClient {
    /// Build a client from a resolved provider. Uses our reqwest 0.13 rustls
    /// client (300s timeout) rather than genai's default builder (which `.expect`s).
    pub fn new(provider: GenaiProvider) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| AppError::External(format!("failed to build reqwest client: {e}")))?;
        let inner = Client::builder().with_reqwest(http).build();
        Ok(Self {
            inner,
            provider,
            hints: RwLock::new(GenerationHints::default()),
        })
    }

    fn snapshot_hints(&self) -> GenerationHints {
        self.hints.read().map(|g| g.clone()).unwrap_or_default()
    }

    fn effective_kind(&self) -> AdapterKind {
        rewrite_openai_kind(self.provider.kind, &self.provider.model)
    }

    fn service_target(&self) -> ServiceTarget {
        let kind = self.effective_kind();
        let model = ModelIden::new(kind, self.provider.model.clone());
        let auth = match kind {
            AdapterKind::Ollama => AuthData::None,
            _ => match &self.provider.api_key {
                Some(key) if !key.is_empty() => AuthData::from_single(key.clone()),
                _ => AuthData::None,
            },
        };
        let endpoint = Endpoint::from_owned(self.resolve_endpoint(kind));
        ServiceTarget {
            endpoint,
            auth,
            model,
        }
    }

    fn resolve_endpoint(&self, kind: AdapterKind) -> String {
        if let Some(url) = self
            .provider
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return ensure_trailing_slash(url);
        }
        default_endpoint(
            kind,
            self.provider.region.as_deref(),
            self.provider.vertex_project.as_deref(),
            self.provider.vertex_location.as_deref(),
            self.provider.custom_index,
        )
    }

    fn chat_options(&self, hints: &GenerationHints, capture_tools: bool) -> ChatOptions {
        let mut opts = ChatOptions::default().with_capture_usage(true);
        if capture_tools {
            opts = opts.with_capture_tool_calls(true);
        }
        let max_tokens = hints.max_tokens.or(self.provider.params.max_tokens);
        if let Some(max) = max_tokens {
            opts = opts.with_max_tokens(max);
        }
        if let Some(temp) = self.provider.params.temperature {
            opts = opts.with_temperature(f64::from(temp));
        }
        if let Some(top_p) = self.provider.params.top_p {
            opts = opts.with_top_p(f64::from(top_p));
        }
        if hints.json_mode {
            opts = opts.with_response_format(ChatResponseFormat::JsonMode);
        }
        if let Some(grammar) = hints.guided_grammar.as_deref() {
            if let Ok(schema) = serde_json::from_str::<serde_json::Value>(grammar) {
                if schema.get("type").is_some() {
                    opts = opts.with_response_format(ChatResponseFormat::JsonSpec(JsonSpec::new(
                        "guided", schema,
                    )));
                }
            }
        }
        if let Some(effort) = hints
            .reasoning_effort
            .as_deref()
            .and_then(ReasoningEffort::from_keyword)
        {
            opts = opts.with_reasoning_effort(effort);
        }
        if let Some(key) = hints.prompt_cache_key.as_ref() {
            opts = opts.with_prompt_cache_key(key.clone());
        }
        if let Some(cc) = hints.cache_control {
            opts = opts.with_cache_control(map_cache(cc));
        }
        if !self.provider.headers.is_empty() {
            opts = opts.with_extra_headers(self.provider.headers.clone());
        }
        let mut extra = serde_json::Map::new();
        if let Some(fp) = self.provider.params.frequency_penalty {
            extra.insert("frequency_penalty".into(), serde_json::json!(fp));
        }
        if let Some(pp) = self.provider.params.presence_penalty {
            extra.insert("presence_penalty".into(), serde_json::json!(pp));
        }
        if hints.suppress_reasoning {
            extra.insert(
                "chat_template_kwargs".into(),
                serde_json::json!({ "enable_thinking": false }),
            );
        }
        if let Some(grammar) = hints.guided_grammar.as_deref() {
            if serde_json::from_str::<serde_json::Value>(grammar)
                .ok()
                .and_then(|v| v.get("type").cloned())
                .is_none()
            {
                extra.insert(
                    "guided_grammar".into(),
                    serde_json::Value::String(grammar.to_string()),
                );
            }
        }
        if !extra.is_empty() {
            opts = opts.with_extra_body(serde_json::Value::Object(extra));
        }
        opts
    }

    async fn exec(
        &self,
        request: ChatRequest,
        hints: &GenerationHints,
        capture_tools: bool,
    ) -> Result<LLMResponse> {
        let target = self.service_target();
        let options = self.chat_options(hints, capture_tools);
        let response = self
            .inner
            .exec_chat(target, request, Some(&options))
            .await
            .map_err(map_error)?;
        Ok(map_response(response))
    }

    async fn exec_stream(
        &self,
        request: ChatRequest,
        hints: &GenerationHints,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let target = self.service_target();
        let options = self.chat_options(hints, false);
        let response = self
            .inner
            .exec_chat_stream(target, request, Some(&options))
            .await
            .map_err(map_error)?;
        let mut inner = response.stream;
        let s = async_stream::stream! {
            while let Some(ev) = inner.next().await {
                match ev {
                    Ok(ChatStreamEvent::Chunk(chunk)) => yield Ok(chunk.content),
                    Ok(_) => {}
                    Err(err) => yield Err(map_error(err)),
                }
            }
        };
        Ok(Box::new(Box::pin(s)))
    }

    async fn exec_stream_with_tools(
        &self,
        request: ChatRequest,
        hints: &GenerationHints,
    ) -> Result<Box<dyn futures::Stream<Item = Result<LlmStreamEvent>> + Send + Unpin>> {
        let target = self.service_target();
        let options = self.chat_options(hints, true);
        let response = self
            .inner
            .exec_chat_stream(target, request, Some(&options))
            .await
            .map_err(map_error)?;
        let mut inner = response.stream;
        let s = async_stream::stream! {
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            while let Some(ev) = inner.next().await {
                match ev {
                    Ok(ChatStreamEvent::Chunk(chunk)) => {
                        yield Ok(LlmStreamEvent::Text(chunk.content));
                    }
                    Ok(ChatStreamEvent::ToolCallChunk(chunk)) => {
                        let tc = chunk.tool_call;
                        tool_calls.push(ToolCall {
                            id: tc.call_id,
                            name: tc.fn_name,
                            arguments: tc.fn_arguments,
                        });
                    }
                    Ok(ChatStreamEvent::End(end)) => {
                        if let Some(captured) = end.captured_into_tool_calls() {
                            if !captured.is_empty() {
                                tool_calls = captured
                                    .into_iter()
                                    .map(|tc| ToolCall {
                                        id: tc.call_id,
                                        name: tc.fn_name,
                                        arguments: tc.fn_arguments,
                                    })
                                    .collect();
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(err) => yield Err(map_error(err)),
                }
            }
            if !tool_calls.is_empty() {
                yield Ok(LlmStreamEvent::ToolCalls(tool_calls));
            }
        };
        Ok(Box::new(Box::pin(s)))
    }
}

#[async_trait]
impl LLMClient for GenaiClient {
    async fn generate(&self, prompt: &str) -> Result<String> {
        Ok(self
            .generate_with_history(&[("user".into(), prompt.to_string())])
            .await?
            .content)
    }

    async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        Ok(self
            .generate_with_history(&[
                ("system".into(), system.to_string()),
                ("user".into(), prompt.to_string()),
            ])
            .await?
            .content)
    }

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<LLMResponse> {
        let hints = self.snapshot_hints();
        let request = request_from_role_content(messages, None, &hints);
        self.exec(request, &hints, false).await
    }

    async fn generate_with_tools(
        &self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        let hints = self.snapshot_hints();
        let request =
            request_from_role_content(&[("user".into(), prompt.to_string())], Some(tools), &hints);
        self.exec(request, &hints, true).await
    }

    async fn generate_with_tools_and_history(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        let hints = self.snapshot_hints();
        let request = request_from_conversation(messages, tools, &hints);
        self.exec(request, &hints, true).await
    }

    async fn stream(
        &self,
        prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let hints = self.snapshot_hints();
        let request =
            request_from_role_content(&[("user".into(), prompt.to_string())], None, &hints);
        self.exec_stream(request, &hints).await
    }

    async fn stream_with_system(
        &self,
        system: &str,
        prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let hints = self.snapshot_hints();
        let request = request_from_role_content(
            &[
                ("system".into(), system.to_string()),
                ("user".into(), prompt.to_string()),
            ],
            None,
            &hints,
        );
        self.exec_stream(request, &hints).await
    }

    async fn stream_with_history(
        &self,
        messages: &[(String, String)],
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let hints = self.snapshot_hints();
        let request = request_from_role_content(messages, None, &hints);
        self.exec_stream(request, &hints).await
    }

    async fn stream_with_tools_and_history(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolDefinition],
    ) -> Result<Box<dyn futures::Stream<Item = Result<LlmStreamEvent>> + Send + Unpin>> {
        let hints = self.snapshot_hints();
        let request = request_from_conversation(messages, tools, &hints);
        self.exec_stream_with_tools(request, &hints).await
    }

    fn model_name(&self) -> &str {
        &self.provider.model
    }

    fn supports_hints(&self) -> bool {
        true
    }

    fn set_hints(&self, hints: GenerationHints) {
        if let Ok(mut slot) = self.hints.write() {
            *slot = hints;
        }
    }

    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let target = self.service_target();
        let response = self
            .inner
            .embed_batch(target, inputs.to_vec(), None)
            .await
            .map_err(map_error)?;
        Ok(response.into_vectors())
    }

    fn supports_vision(&self) -> bool {
        true
    }

    fn supports_provider_web_search(&self) -> bool {
        true
    }
}

/// `type = openai` + gpt-5* / gpt*codex / gpt*pro → OpenAI Responses. Other kinds stay put.
pub(crate) fn rewrite_openai_kind(kind: AdapterKind, model: &str) -> AdapterKind {
    if kind != AdapterKind::OpenAI {
        return kind;
    }
    if model.starts_with("gpt-5")
        || (model.starts_with("gpt") && (model.contains("codex") || model.contains("pro")))
    {
        AdapterKind::OpenAIResp
    } else {
        kind
    }
}

/// Concatenate text parts; non-text parts are ignored.
pub(crate) fn join_parts(parts: &[AresPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            AresPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Text for a system message: `content` plus any `Text` parts, in that order.
///
/// System prompts are text-only on every provider; binary parts are ignored.
/// The typed `content` is kept unless a `Text` part already carries it, so a
/// caller that passes the prompt in `content` and extras in `parts` does not
/// lose the prompt (same rule as [`ares_parts_to_genai`]).
pub(crate) fn system_text(parts: &[AresPart], content: &str) -> String {
    if parts.is_empty() {
        return content.to_string();
    }
    let joined = join_parts(parts);
    let covered = parts
        .iter()
        .any(|p| matches!(p, AresPart::Text { text } if text == content));
    if content.is_empty() || covered {
        joined
    } else if joined.is_empty() {
        content.to_string()
    } else {
        format!("{content}\n{joined}")
    }
}

/// Map ARES tool definitions to genai tools, stripping `provider_web_search`
/// and injecting [`ToolName::WebSearch`] when requested.
pub(crate) fn map_tools(tools: &[ToolDefinition], web_search: bool) -> Vec<Tool> {
    let has_provider = tools.iter().any(|t| t.name == PROVIDER_WEB_SEARCH);
    let mut out: Vec<Tool> = tools
        .iter()
        .filter(|t| t.name != PROVIDER_WEB_SEARCH)
        .map(|t| {
            Tool::new(t.name.clone())
                .with_description(t.description.clone())
                .with_schema(t.parameters.clone())
        })
        .collect();
    if has_provider || web_search {
        out.push(Tool::new_web_search());
    }
    out
}

fn request_from_role_content(
    messages: &[(String, String)],
    tools: Option<&[ToolDefinition]>,
    hints: &GenerationHints,
) -> ChatRequest {
    let mut system = String::new();
    let mut chat_messages = Vec::new();
    for (role, content) in messages {
        match role.as_str() {
            "system" => {
                if !system.is_empty() {
                    system.push('\n');
                }
                system.push_str(content);
            }
            "assistant" => chat_messages.push(ChatMessage::assistant(content.clone())),
            "tool" => chat_messages.push(ChatMessage::tool(content.clone())),
            _ => chat_messages.push(ChatMessage::user(content.clone())),
        }
    }
    finish_request(system, chat_messages, tools, hints, None, None)
}

fn request_from_conversation(
    messages: &[ConversationMessage],
    tools: &[ToolDefinition],
    hints: &GenerationHints,
) -> ChatRequest {
    let mut system = String::new();
    let mut chat_messages = Vec::new();
    let mut prev_id = hints.previous_response_id.clone();
    let mut store = hints.store;
    for msg in messages {
        if prev_id.is_none() {
            prev_id = msg.previous_response_id.clone();
        }
        if store.is_none() {
            store = msg.store;
        }
        match msg.role {
            MessageRole::System => {
                if !system.is_empty() {
                    system.push('\n');
                }
                system.push_str(&system_text(&msg.parts, &msg.content));
            }
            MessageRole::User => {
                let mut message = ChatMessage::user(parts_to_content(&msg.parts, &msg.content));
                if let Some(cc) = msg.cache_control {
                    message = message.with_options(MessageOptions {
                        cache_control: Some(map_cache(cc)),
                    });
                }
                chat_messages.push(message);
            }
            MessageRole::Assistant => {
                let mut parts = ares_parts_to_genai(&msg.parts, &msg.content);
                if let Some(reason) = msg.reasoning_content.as_ref() {
                    parts.push(ContentPart::ReasoningContent(reason.clone()));
                }
                for call in &msg.tool_calls {
                    parts.push(ContentPart::ToolCall(genai::chat::ToolCall {
                        call_id: call.id.clone(),
                        fn_name: call.name.clone(),
                        fn_arguments: call.arguments.clone(),
                        thought_signatures: None,
                    }));
                }
                let mut message = ChatMessage::assistant(MessageContent::from_parts(parts));
                if let Some(cc) = msg.cache_control {
                    message = message.with_options(MessageOptions {
                        cache_control: Some(map_cache(cc)),
                    });
                }
                chat_messages.push(message);
            }
            MessageRole::Tool => {
                let response = ToolResponse::new(
                    msg.tool_call_id.clone().unwrap_or_default(),
                    msg.content.clone(),
                );
                chat_messages.push(ChatMessage::from(response));
            }
        }
    }
    finish_request(system, chat_messages, Some(tools), hints, prev_id, store)
}

fn finish_request(
    system: String,
    messages: Vec<ChatMessage>,
    tools: Option<&[ToolDefinition]>,
    hints: &GenerationHints,
    previous_response_id: Option<String>,
    store: Option<bool>,
) -> ChatRequest {
    let mut request = ChatRequest::new(messages);
    if !system.is_empty() {
        request = request.with_system(system);
    }
    let mapped = match tools {
        Some(defs) => map_tools(defs, hints.web_search),
        None if hints.web_search => vec![Tool::new_web_search()],
        None => Vec::new(),
    };
    if !mapped.is_empty() {
        request = request.with_tools(mapped);
    }
    if let Some(id) = previous_response_id.or_else(|| hints.previous_response_id.clone()) {
        request = request.with_previous_response_id(id);
    }
    if let Some(store) = store.or(hints.store) {
        request = request.with_store(store);
    }
    request
}

fn parts_to_content(parts: &[AresPart], fallback: &str) -> MessageContent {
    MessageContent::from_parts(ares_parts_to_genai(parts, fallback))
}

fn ares_parts_to_genai(parts: &[AresPart], fallback: &str) -> Vec<ContentPart> {
    if parts.is_empty() {
        return if fallback.is_empty() {
            Vec::new()
        } else {
            vec![ContentPart::Text(fallback.to_string())]
        };
    }
    let mut out: Vec<ContentPart> =
        parts
            .iter()
            .map(|part| match part {
                AresPart::Text { text } => ContentPart::Text(text.clone()),
                AresPart::ImageUrl { url } => {
                    ContentPart::Binary(Binary::from_url("image/*", url.clone(), None))
                }
                AresPart::ImageBase64 { mime, data } => {
                    ContentPart::Binary(Binary::from_base64(mime.clone(), data.clone(), None))
                }
                AresPart::FileUrl { url, mime } => ContentPart::Binary(Binary::from_url(
                    mime.clone()
                        .unwrap_or_else(|| "application/octet-stream".into()),
                    url.clone(),
                    None,
                )),
                AresPart::FileBase64 { mime, data, name } => ContentPart::Binary(
                    Binary::from_base64(mime.clone(), data.clone(), name.clone()),
                ),
            })
            .collect();
    // Callers pass the typed prompt as `content` and attachments as `parts`
    // (e.g. HTTP chat). Keep the prompt unless a Text part already covers it,
    // otherwise it is silently dropped and only binaries reach the provider.
    if !fallback.is_empty() && !parts.iter().any(|p| matches!(p, AresPart::Text { .. })) {
        out.insert(0, ContentPart::Text(fallback.to_string()));
    }
    out
}

fn map_response(response: ChatResponse) -> LLMResponse {
    let tool_calls: Vec<ToolCall> = response
        .tool_calls()
        .into_iter()
        .map(|tc| ToolCall {
            id: tc.call_id.clone(),
            name: tc.fn_name.clone(),
            arguments: tc.fn_arguments.clone(),
        })
        .collect();
    let finish_reason = response
        .stop_reason
        .as_ref()
        .map(|r| r.raw().to_string())
        .unwrap_or_else(|| {
            if tool_calls.is_empty() {
                "stop".into()
            } else {
                "tool_calls".into()
            }
        });
    LLMResponse {
        content: response.first_text().unwrap_or("").to_string(),
        tool_calls,
        finish_reason,
        usage: map_usage(&response.usage),
        reasoning_content: response.reasoning_content,
        response_id: response.response_id,
    }
}

fn map_usage(usage: &Usage) -> Option<TokenUsage> {
    if usage.prompt_tokens.is_none()
        && usage.completion_tokens.is_none()
        && usage.total_tokens.is_none()
    {
        return None;
    }
    let prompt = usage.prompt_tokens.unwrap_or(0).max(0) as u32;
    let completion = usage.completion_tokens.unwrap_or(0).max(0) as u32;
    let total = usage
        .total_tokens
        .map(|n| n.max(0) as u32)
        .unwrap_or(prompt.saturating_add(completion));
    Some(TokenUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        cached_tokens: usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
            .map(|n| i64::from(n.max(0))),
    })
}

fn map_cache(cc: CacheControl) -> GenaiCache {
    match cc {
        CacheControl::Ephemeral => GenaiCache::Ephemeral,
        CacheControl::Ephemeral5m => GenaiCache::Ephemeral5m,
        CacheControl::Ephemeral24h => GenaiCache::Ephemeral24h,
    }
}

fn map_error(err: genai::Error) -> AppError {
    let status = err.status();
    let message = match status {
        Some(code) => format!("HTTP {code}: {err}"),
        None => err.to_string(),
    };
    if status.map(|s| s.as_u16()) == Some(429) {
        AppError::RateLimited(message)
    } else {
        AppError::LLM(message)
    }
}

fn ensure_trailing_slash(url: &str) -> String {
    if url.ends_with('/') {
        url.to_string()
    } else {
        format!("{url}/")
    }
}

fn default_endpoint(
    kind: AdapterKind,
    region: Option<&str>,
    vertex_project: Option<&str>,
    vertex_location: Option<&str>,
    custom_index: Option<u8>,
) -> String {
    match kind {
        AdapterKind::OpenAI | AdapterKind::OpenAIResp => "https://api.openai.com/v1/".into(),
        AdapterKind::Gemini => "https://generativelanguage.googleapis.com/v1beta/".into(),
        AdapterKind::Anthropic => "https://api.anthropic.com/v1/".into(),
        AdapterKind::MiniMax => "https://api.minimax.io/anthropic/v1/".into(),
        AdapterKind::Ollama => "http://localhost:11434/".into(),
        AdapterKind::OllamaCloud => "https://ollama.com/".into(),
        AdapterKind::Cohere => "https://api.cohere.com/v1/".into(),
        AdapterKind::Fireworks => "https://api.fireworks.ai/inference/v1/".into(),
        AdapterKind::Together => "https://api.together.xyz/v1/".into(),
        AdapterKind::Groq => "https://api.groq.com/openai/v1/".into(),
        AdapterKind::DeepSeek => "https://api.deepseek.com/v1/".into(),
        AdapterKind::Xai => "https://api.x.ai/v1/".into(),
        AdapterKind::Aihubmix => "https://aihubmix.com/v1/".into(),
        AdapterKind::Kimi => "https://api.moonshot.ai/v1/".into(),
        AdapterKind::Moonshot => "https://api.moonshot.cn/v1/".into(),
        AdapterKind::Nebius => "https://api.studio.nebius.ai/v1/".into(),
        AdapterKind::Mimo => "https://api.mimo.com/openai/v1/".into(),
        AdapterKind::Zai => "https://api.z.ai/api/paas/v4/".into(),
        AdapterKind::BigModel => "https://open.bigmodel.cn/api/paas/v4/".into(),
        AdapterKind::Aliyun => "https://dashscope.aliyuncs.com/compatible-mode/v1/".into(),
        AdapterKind::QwenCloud => "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/".into(),
        AdapterKind::OpenRouter => "https://openrouter.ai/api/v1/".into(),
        AdapterKind::AtlasCloud => "https://api.atlascloud.ai/v1/".into(),
        AdapterKind::GithubCopilot => "https://models.github.ai/inference/".into(),
        AdapterKind::OpenCodeGo => "https://opencode.ai/zen/go/v1/".into(),
        AdapterKind::BedrockApi => {
            let region = region
                .map(str::to_string)
                .or_else(|| std::env::var("AWS_REGION").ok())
                .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
                .unwrap_or_else(|| "us-east-1".into());
            format!("https://bedrock-runtime.{region}.amazonaws.com/")
        }
        AdapterKind::Vertex => {
            let project = vertex_project
                .map(str::to_string)
                .or_else(|| std::env::var("VERTEX_PROJECT_ID").ok())
                .unwrap_or_default();
            match vertex_location
                .map(str::to_string)
                .or_else(|| std::env::var("VERTEX_LOCATION").ok())
            {
                Some(loc) if !loc.is_empty() && loc != "global" => {
                    format!(
                        "https://{loc}-aiplatform.googleapis.com/v1/projects/{project}/locations/{loc}/"
                    )
                }
                _ => format!(
                    "https://aiplatform.googleapis.com/v1/projects/{project}/locations/global/"
                ),
            }
        }
        AdapterKind::Baidu => "https://qianfan.baidubce.com/v2/".into(),
        AdapterKind::Omlx => std::env::var("OMLX_ENDPOINT")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| ensure_trailing_slash(&s))
            .unwrap_or_else(|| "http://127.0.0.1:8000/v1/".into()),
        AdapterKind::Custom(n) => {
            let idx = custom_index.unwrap_or(n);
            std::env::var(format!("GENAI_{idx}_ENDPOINT"))
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| ensure_trailing_slash(&s))
                .unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpt5_kind_rewrite_only_for_openai() {
        assert_eq!(
            rewrite_openai_kind(AdapterKind::OpenAI, "gpt-5"),
            AdapterKind::OpenAIResp
        );
        assert_eq!(
            rewrite_openai_kind(AdapterKind::OpenAI, "gpt-5-mini"),
            AdapterKind::OpenAIResp
        );
        assert_eq!(
            rewrite_openai_kind(AdapterKind::OpenAI, "gpt-4o-codex"),
            AdapterKind::OpenAIResp
        );
        assert_eq!(
            rewrite_openai_kind(AdapterKind::OpenAI, "gpt-4.1-pro"),
            AdapterKind::OpenAIResp
        );
        assert_eq!(
            rewrite_openai_kind(AdapterKind::OpenAI, "gpt-4o"),
            AdapterKind::OpenAI
        );
        assert_eq!(
            rewrite_openai_kind(AdapterKind::Anthropic, "gpt-5"),
            AdapterKind::Anthropic
        );
        assert_eq!(
            rewrite_openai_kind(AdapterKind::OpenAI, "o3-mini"),
            AdapterKind::OpenAI
        );
    }

    #[test]
    fn join_parts_concatenates_text() {
        let parts = vec![
            AresPart::Text {
                text: "hello ".into(),
            },
            AresPart::ImageUrl {
                url: "https://example.com/x.png".into(),
            },
            AresPart::Text {
                text: "world".into(),
            },
        ];
        assert_eq!(join_parts(&parts), "hello world");
        assert_eq!(join_parts(&[]), "");
    }

    #[test]
    fn content_fallback_kept_when_parts_have_no_text() {
        let parts = vec![AresPart::ImageBase64 {
            mime: "image/png".into(),
            data: "AAAA".into(),
        }];
        let out = ares_parts_to_genai(&parts, "describe this");
        assert_eq!(out.len(), 2, "fallback text must be prepended");
        assert!(matches!(&out[0], ContentPart::Text(t) if t == "describe this"));
        assert!(matches!(out[1], ContentPart::Binary(_)));
    }

    #[test]
    fn content_fallback_not_duplicated_when_text_part_present() {
        let parts = vec![
            AresPart::Text {
                text: "typed prompt".into(),
            },
            AresPart::ImageBase64 {
                mime: "image/png".into(),
                data: "AAAA".into(),
            },
        ];
        let out = ares_parts_to_genai(&parts, "typed prompt");
        let texts = out
            .iter()
            .filter(|p| matches!(p, ContentPart::Text(_)))
            .count();
        assert_eq!(texts, 1, "content fallback must not duplicate a Text part");
    }

    #[test]
    fn content_fallback_skipped_when_empty() {
        let parts = vec![AresPart::ImageBase64 {
            mime: "image/png".into(),
            data: "AAAA".into(),
        }];
        let out = ares_parts_to_genai(&parts, "");
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], ContentPart::Binary(_)));
    }

    #[test]
    fn system_text_keeps_content_with_non_text_parts() {
        let parts = vec![AresPart::ImageBase64 {
            mime: "image/png".into(),
            data: "AAAA".into(),
        }];
        assert_eq!(system_text(&parts, "be terse"), "be terse");
    }

    #[test]
    fn system_text_appends_text_parts_after_content() {
        let parts = vec![AresPart::Text {
            text: "extra rule".into(),
        }];
        assert_eq!(system_text(&parts, "be terse"), "be terse\nextra rule");
    }

    #[test]
    fn system_text_does_not_duplicate_covering_text_part() {
        let parts = vec![AresPart::Text {
            text: "be terse".into(),
        }];
        assert_eq!(system_text(&parts, "be terse"), "be terse");
        assert_eq!(system_text(&[], "be terse"), "be terse");
        assert_eq!(system_text(&parts, ""), "be terse");
    }

    #[test]
    fn provider_web_search_is_stripped_and_replaced() {
        let tools = vec![
            ToolDefinition {
                name: "lookup".into(),
                description: "lookup".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
            ToolDefinition {
                name: PROVIDER_WEB_SEARCH.into(),
                description: "search".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        ];
        let mapped = map_tools(&tools, false);
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].name.as_str(), "lookup");
        assert!(matches!(mapped[1].name, genai::chat::ToolName::WebSearch));
        assert!(mapped
            .iter()
            .all(|t| t.name.as_str() != PROVIDER_WEB_SEARCH));

        let hint_only = map_tools(&[], true);
        assert_eq!(hint_only.len(), 1);
        assert!(matches!(
            hint_only[0].name,
            genai::chat::ToolName::WebSearch
        ));

        let none = map_tools(&[], false);
        assert!(none.is_empty());
    }

    #[test]
    fn request_from_conversation_maps_parts() {
        let mut msg = ConversationMessage::user("fallback-text");
        msg.parts = vec![
            AresPart::Text {
                text: "hello ".into(),
            },
            AresPart::ImageUrl {
                url: "https://example.com/x.png".into(),
            },
            AresPart::Text {
                text: "world".into(),
            },
        ];
        let req = request_from_conversation(&[msg], &[], &GenerationHints::default());
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].content.texts(), vec!["hello ", "world"]);
        assert!(matches!(
            req.messages[0].content.parts()[1],
            ContentPart::Binary(_)
        ));
    }

    #[test]
    fn llm_stream_event_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<LlmStreamEvent>();
    }
}
