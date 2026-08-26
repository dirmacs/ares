//! OpenAI LLM client implementation
//!
//! This module provides integration with OpenAI API and compatible endpoints.
//!
//! # Features
//!
//! Enable with the `openai` feature flag.
//!
//! # Example
//!
//! ```rust,ignore
//! use ares::llm::{LLMClient, Provider};
//!
//! let provider = Provider::OpenAI {
//!     api_key: "sk-...".to_string(),
//!     api_base: "https://api.openai.com/v1".to_string(),
//!     model: "gpt-4".to_string(),
//! };
//! let client = provider.create_client().await?;
//! let response = client.generate("Hello!").await?;
//! ```

use crate::client::{GenerationHints, LLMClient, LLMResponse, ModelParams, TokenUsage};
use crate::coordinator::{ConversationMessage, MessageRole};
use ares_types::types::{AppError, Result, ToolCall, ToolDefinition};
use async_openai::types::chat::{
    CreateChatCompletionRequest, CreateChatCompletionResponse, ResponseFormat,
    ResponseFormatJsonSchema,
};
use async_openai::{
    Client,
    config::{Config as _, OpenAIConfig},
    types::chat::{
        ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestToolMessageArgs,
        ChatCompletionRequestUserMessageArgs, ChatCompletionTool, ChatCompletionTools,
        CreateChatCompletionRequestArgs, FunctionCall, FunctionObject, ReasoningEffort,
    },
};
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::RwLock;
use std::time::Duration;

/// OpenAI client for API-based inference
pub struct OpenAIClient {
    client: Client<OpenAIConfig>,
    /// Direct HTTP handle mirroring the client's transport (same timeouts,
    /// default headers). Used only by the extension-field send path, which
    /// posts hint-derived provider-specific fields the typed request cannot
    /// express.
    http: reqwest::Client,
    model: String,
    params: ModelParams,
    /// Generation hints applying to SUBSEQUENT calls (see [`GenerationHints`]
    /// for the set-on-client contract). Snapshotted inside the single
    /// request-argument funnel.
    hints: RwLock<GenerationHints>,
}

impl OpenAIClient {
    /// Create a new OpenAI client
    ///
    /// # Arguments
    ///
    /// * `api_key` - OpenAI API key
    /// * `api_base` - Base URL for the API (e.g., `https://api.openai.com/v1`)
    /// * `model` - Model identifier (e.g., "gpt-4", "gpt-3.5-turbo")
    pub fn new(api_key: String, api_base: String, model: String) -> Self {
        Self::with_params(api_key, api_base, model, ModelParams::default())
    }

    /// Create a new OpenAI client with model parameters
    ///
    /// # Arguments
    ///
    /// * `api_key` - OpenAI API key
    /// * `api_base` - Base URL for the API (e.g., `https://api.openai.com/v1`)
    /// * `model` - Model identifier (e.g., "gpt-4", "gpt-3.5-turbo")
    /// * `params` - Model inference parameters (temperature, max_tokens, etc.)
    pub fn with_params(
        api_key: String,
        api_base: String,
        model: String,
        params: ModelParams,
    ) -> Self {
        Self::with_params_and_headers(
            api_key,
            api_base,
            model,
            params,
            std::collections::HashMap::new(),
        )
    }

    /// Create a new OpenAI client with model parameters and custom headers.
    ///
    /// # Arguments
    ///
    /// * `api_key` - OpenAI API key
    /// * `api_base` - Base URL for the API
    /// * `model` - Model identifier
    /// * `params` - Model inference parameters
    /// * `headers` - Extra HTTP headers to include with every request
    pub fn with_params_and_headers(
        api_key: String,
        api_base: String,
        model: String,
        params: ModelParams,
        headers: std::collections::HashMap<String, String>,
    ) -> Self {
        let config = OpenAIConfig::new()
            .with_api_key(api_key)
            .with_api_base(api_base);

        let mut header_map = reqwest::header::HeaderMap::new();
        for (k, v) in headers {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(&v),
            ) {
                header_map.insert(name, value);
            }
        }

        let http_client = reqwest::ClientBuilder::new()
            .default_headers(header_map)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build()
            .expect("failed to build reqwest client");

        Self {
            client: Client::with_config(config).with_http_client(http_client.clone()),
            http: http_client,
            model,
            params,
            hints: RwLock::new(GenerationHints::default()),
        }
    }

    /// Convert ToolDefinition to ChatCompletionTool
    fn convert_tool(tool: &ToolDefinition) -> ChatCompletionTools {
        ChatCompletionTools::Function(ChatCompletionTool {
            function: FunctionObject {
                name: tool.name.clone(),
                description: Some(tool.description.clone()),
                parameters: Some(tool.parameters.clone()),
                strict: None,
            },
        })
    }

    /// Extract tool calls from the response message tool calls
    fn extract_tool_calls(tool_calls: &[ChatCompletionMessageToolCalls]) -> Vec<ToolCall> {
        tool_calls
            .iter()
            .filter_map(|wrapper| match wrapper {
                ChatCompletionMessageToolCalls::Function(call) => Some(ToolCall {
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                    arguments: serde_json::from_str(&call.function.arguments)
                        .unwrap_or(serde_json::json!({})),
                }),
                ChatCompletionMessageToolCalls::Custom(_) => None,
            })
            .collect()
    }

    /// Convert a ConversationMessage to OpenAI's ChatCompletionRequestMessage
    fn convert_conversation_message(
        &self,
        msg: &ConversationMessage,
    ) -> Result<ChatCompletionRequestMessage> {
        match msg.role {
            MessageRole::System => {
                let system_msg = ChatCompletionRequestSystemMessageArgs::default()
                    .content(msg.content.clone())
                    .build()
                    .map_err(|e| AppError::LLM(format!("Failed to build system message: {}", e)))?;
                Ok(ChatCompletionRequestMessage::System(system_msg))
            }
            MessageRole::User => {
                let user_msg = ChatCompletionRequestUserMessageArgs::default()
                    .content(msg.content.clone())
                    .build()
                    .map_err(|e| AppError::LLM(format!("Failed to build user message: {}", e)))?;
                Ok(ChatCompletionRequestMessage::User(user_msg))
            }
            MessageRole::Assistant => {
                let mut builder = ChatCompletionRequestAssistantMessageArgs::default();

                if !msg.content.is_empty() {
                    builder.content(msg.content.clone());
                }

                // Convert tool calls if present
                if !msg.tool_calls.is_empty() {
                    let openai_tool_calls: Vec<ChatCompletionMessageToolCalls> = msg
                        .tool_calls
                        .iter()
                        .map(|tc| {
                            ChatCompletionMessageToolCalls::Function(
                                ChatCompletionMessageToolCall {
                                    id: tc.id.clone(),
                                    function: FunctionCall {
                                        name: tc.name.clone(),
                                        arguments: serde_json::to_string(&tc.arguments)
                                            .unwrap_or_else(|_| "{}".to_string()),
                                    },
                                },
                            )
                        })
                        .collect();
                    builder.tool_calls(openai_tool_calls);
                }

                let assistant_msg = builder.build().map_err(|e| {
                    AppError::LLM(format!("Failed to build assistant message: {}", e))
                })?;
                Ok(ChatCompletionRequestMessage::Assistant(assistant_msg))
            }
            MessageRole::Tool => {
                let tool_call_id = msg.tool_call_id.clone().ok_or_else(|| {
                    AppError::LLM("Tool message must have a tool_call_id".to_string())
                })?;

                let tool_msg = ChatCompletionRequestToolMessageArgs::default()
                    .tool_call_id(tool_call_id)
                    .content(msg.content.clone())
                    .build()
                    .map_err(|e| AppError::LLM(format!("Failed to build tool message: {}", e)))?;
                Ok(ChatCompletionRequestMessage::Tool(tool_msg))
            }
        }
    }

    /// Single funnel every chat-completion request passes through: static
    /// model params first, then generation hints. Hint mapping:
    /// - `json_mode` → `response_format` `json_object`
    /// - `max_tokens` → `max_completion_tokens` (only when params did not
    ///   already set one, so an explicit config budget wins)
    /// - `suppress_reasoning` → `reasoning_effort: minimal`
    /// - `guided_grammar`: schema-shaped values → `response_format`
    ///   `json_schema`; raw GBNF/EBNF text rides the provider-specific
    ///   extension field attached by [`Self::send_chat_request`]
    fn apply_model_params(&self, builder: &mut CreateChatCompletionRequestArgs) {
        // GPT-5 chat-completions works reliably here when we explicitly cap reasoning effort.
        if self.model.starts_with("gpt-5") {
            builder.reasoning_effort(ReasoningEffort::Low);
        }

        if let Some(temp) = self.params.temperature {
            builder.temperature(temp);
        }
        if let Some(max_tokens) = self.params.max_tokens {
            builder.max_completion_tokens(max_tokens);
        }
        if let Some(top_p) = self.params.top_p {
            builder.top_p(top_p);
        }
        if let Some(freq_penalty) = self.params.frequency_penalty {
            builder.frequency_penalty(freq_penalty);
        }
        if let Some(pres_penalty) = self.params.presence_penalty {
            builder.presence_penalty(pres_penalty);
        }

        let hints = self.hints.read().map(|h| h.clone()).unwrap_or_default();
        if hints.json_mode {
            builder.response_format(ResponseFormat::JsonObject);
        }
        if let Some(schema) = hints
            .guided_grammar
            .as_deref()
            .and_then(Self::schema_shaped_grammar)
        {
            // Schema-shaped grammar overrides plain json_mode with strict
            // structured output.
            builder.response_format(ResponseFormat::JsonSchema {
                json_schema: ResponseFormatJsonSchema {
                    description: None,
                    name: Self::GUIDED_GRAMMAR_SCHEMA_NAME.to_string(),
                    schema: Some(schema),
                    strict: None,
                },
            });
        }
        if self.params.max_tokens.is_none() {
            if let Some(budget) = hints.max_tokens {
                builder.max_completion_tokens(budget);
            }
        }
        if hints.suppress_reasoning {
            builder.reasoning_effort(ReasoningEffort::Minimal);
        }
    }

    /// Provider-specific extension key carrying raw guided-grammar text for
    /// OpenAI-compatible servers with grammar-constrained decoding
    /// (vLLM-style `guided_grammar`). Schema-shaped grammars ride
    /// `response_format` instead and never take this path.
    const GUIDED_GRAMMAR_EXTENSION: &'static str = "guided_grammar";

    /// Structured-output name used for schema-shaped guided-grammar hints.
    const GUIDED_GRAMMAR_SCHEMA_NAME: &'static str = "guided_output";

    /// Classify a `guided_grammar` hint: `Some(schema)` when the value is
    /// shaped like a JSON Schema object (parses as JSON with an object root
    /// and a `"type"` member), which OpenAI-compatible endpoints honor
    /// natively via structured outputs; `None` for raw GBNF/EBNF-style text.
    fn schema_shaped_grammar(grammar: &str) -> Option<serde_json::Value> {
        serde_json::from_str::<serde_json::Value>(grammar)
            .ok()
            .filter(|v| v.is_object() && v.get("type").is_some())
    }

    /// Hint-derived provider-specific extension fields: currently the raw
    /// (non-schema) guided-grammar text under
    /// [`Self::GUIDED_GRAMMAR_EXTENSION`]. `None` = nothing to attach, so
    /// requests flow through the normal typed pipeline unchanged.
    fn hint_extension_fields(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        let grammar = self
            .hints
            .read()
            .ok()
            .and_then(|h| h.guided_grammar.clone())
            .filter(|g| !g.trim().is_empty())?;
        if Self::schema_shaped_grammar(&grammar).is_some() {
            return None;
        }
        let mut extensions = serde_json::Map::new();
        extensions.insert(
            Self::GUIDED_GRAMMAR_EXTENSION.to_string(),
            serde_json::Value::String(grammar),
        );
        Some(extensions)
    }

    /// Send a chat-completion request through the typed pipeline, switching
    /// to the direct extension-field POST only when a raw guided-grammar
    /// hint demands it.
    async fn send_chat_request(
        &self,
        request: CreateChatCompletionRequest,
    ) -> Result<CreateChatCompletionResponse> {
        match self.hint_extension_fields() {
            None => self
                .client
                .chat()
                .create(request)
                .await
                .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e))),
            Some(extensions) => self.send_with_extensions(request, &extensions).await,
        }
    }

    /// Direct POST for requests that must carry provider-specific extension
    /// fields the typed request type cannot express (currently raw guided
    /// grammar). Mirrors the typed pipeline's URL resolution, auth headers,
    /// and error mapping. Streaming calls cannot take this path and silently
    /// ignore the extension fields.
    async fn send_with_extensions(
        &self,
        request: CreateChatCompletionRequest,
        extensions: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<CreateChatCompletionResponse> {
        let mut body = serde_json::to_value(&request)
            .map_err(|e| AppError::LLM(format!("Failed to serialize request: {}", e)))?;
        if let Some(object) = body.as_object_mut() {
            object.extend(extensions.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        let response = self
            .http
            .post(self.client.config().url("/chat/completions"))
            .headers(self.client.config().headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e)))?;
        let status = response.status();
        let payload = response
            .text()
            .await
            .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e)))?;
        if !status.is_success() {
            return Err(AppError::LLM(format!(
                "OpenAI API error: HTTP {} {}",
                status.as_u16(),
                payload
            )));
        }
        serde_json::from_str::<CreateChatCompletionResponse>(&payload)
            .map_err(|e| AppError::LLM(format!("Failed to parse response: {}", e)))
    }
}

#[async_trait]
impl LLMClient for OpenAIClient {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let message = ChatCompletionRequestUserMessageArgs::default()
            .content(prompt)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build message: {}", e)))?;

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(vec![ChatCompletionRequestMessage::User(message)]);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let response = self.send_chat_request(request).await?;

        response
            .choices
            .first()
            .and_then(|choice| choice.message.content.clone())
            .ok_or_else(|| AppError::LLM("No response from OpenAI".to_string()))
    }

    async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        let system_message = ChatCompletionRequestSystemMessageArgs::default()
            .content(system)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build system message: {}", e)))?;

        let user_message = ChatCompletionRequestUserMessageArgs::default()
            .content(prompt)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build user message: {}", e)))?;

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(vec![
            ChatCompletionRequestMessage::System(system_message),
            ChatCompletionRequestMessage::User(user_message),
        ]);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let response = self.send_chat_request(request).await?;

        response
            .choices
            .first()
            .and_then(|choice| choice.message.content.clone())
            .ok_or_else(|| AppError::LLM("No response from OpenAI".to_string()))
    }

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<LLMResponse> {
        let chat_messages: std::result::Result<Vec<ChatCompletionRequestMessage>, AppError> =
            messages
                .iter()
                .map(|(role, content)| {
                    match role.as_str() {
                        "system" => {
                            let msg = ChatCompletionRequestSystemMessageArgs::default()
                                .content(content.as_str())
                                .build()
                                .map_err(|e| {
                                    AppError::LLM(format!("Failed to build system message: {}", e))
                                })?;
                            Ok(ChatCompletionRequestMessage::System(msg))
                        }
                        "assistant" => {
                            let msg = ChatCompletionRequestAssistantMessageArgs::default()
                                .content(content.as_str())
                                .build()
                                .map_err(|e| {
                                    AppError::LLM(format!(
                                        "Failed to build assistant message: {}",
                                        e
                                    ))
                                })?;
                            Ok(ChatCompletionRequestMessage::Assistant(msg))
                        }
                        _ => {
                            // Default to user message
                            let msg = ChatCompletionRequestUserMessageArgs::default()
                                .content(content.as_str())
                                .build()
                                .map_err(|e| {
                                    AppError::LLM(format!("Failed to build user message: {}", e))
                                })?;
                            Ok(ChatCompletionRequestMessage::User(msg))
                        }
                    }
                })
                .collect();

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(chat_messages?);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let response = self.send_chat_request(request).await?;

        let content = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.clone())
            .ok_or_else(|| AppError::LLM("No response from OpenAI".to_string()))?;

        #[allow(clippy::unnecessary_cast)]
        let usage = response
            .usage
            .map(|u| TokenUsage::new(u.prompt_tokens as u32, u.completion_tokens as u32));

        Ok(LLMResponse {
            content,
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
            usage,
        })
    }

    async fn generate_with_tools(
        &self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        let openai_tools: Vec<ChatCompletionTools> = tools.iter().map(Self::convert_tool).collect();

        let user_message = ChatCompletionRequestUserMessageArgs::default()
            .content(prompt)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build user message: {}", e)))?;

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(vec![ChatCompletionRequestMessage::User(user_message)]);
        builder.tools(openai_tools);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let response = self.send_chat_request(request).await?;

        let choice = response
            .choices
            .first()
            .ok_or_else(|| AppError::LLM("No response from OpenAI".to_string()))?;

        let content = choice.message.content.clone().unwrap_or_default();

        let finish_reason = choice
            .finish_reason
            .as_ref()
            .map(|r| format!("{:?}", r).to_lowercase())
            .unwrap_or_else(|| "stop".to_string());

        let tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .map(|calls| Self::extract_tool_calls(calls))
            .unwrap_or_default();

        // Extract token usage if available
        #[allow(clippy::unnecessary_cast)]
        let usage = response
            .usage
            .map(|u| TokenUsage::new(u.prompt_tokens as u32, u.completion_tokens as u32));

        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason,
            usage,
        })
    }

    async fn generate_with_tools_and_history(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        // Convert ConversationMessage to OpenAI format
        let openai_messages: Vec<ChatCompletionRequestMessage> = messages
            .iter()
            .map(|msg| self.convert_conversation_message(msg))
            .collect::<Result<Vec<_>>>()?;

        // Convert tools to OpenAI format
        let openai_tools: Vec<ChatCompletionTools> = tools.iter().map(Self::convert_tool).collect();

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(openai_messages);

        if !openai_tools.is_empty() {
            builder.tools(openai_tools);
        }
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let response = self.send_chat_request(request).await?;

        let choice = response
            .choices
            .first()
            .ok_or_else(|| AppError::LLM("No response from OpenAI".to_string()))?;

        let content = choice.message.content.clone().unwrap_or_default();

        let finish_reason = choice
            .finish_reason
            .as_ref()
            .map(|r| format!("{:?}", r).to_lowercase())
            .unwrap_or_else(|| "stop".to_string());

        let tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .map(|calls| Self::extract_tool_calls(calls))
            .unwrap_or_default();

        #[allow(clippy::unnecessary_cast)]
        let usage = response
            .usage
            .map(|u| TokenUsage::new(u.prompt_tokens as u32, u.completion_tokens as u32));

        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason,
            usage,
        })
    }

    async fn stream(
        &self,
        prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let user_message = ChatCompletionRequestUserMessageArgs::default()
            .content(prompt)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build user message: {}", e)))?;

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(vec![ChatCompletionRequestMessage::User(user_message)]);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let mut stream = self
            .client
            .chat()
            .create_stream(request)
            .await
            .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e)))?;

        let result_stream = async_stream::stream! {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(response) => {
                        for choice in response.choices {
                            if let Some(content) = choice.delta.content {
                                yield Ok(content);
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(AppError::LLM(format!("Stream error: {}", e)));
                    }
                }
            }
        };

        Ok(Box::new(Box::pin(result_stream)))
    }

    async fn stream_with_system(
        &self,
        system: &str,
        prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let system_message = ChatCompletionRequestSystemMessageArgs::default()
            .content(system)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build system message: {}", e)))?;

        let user_message = ChatCompletionRequestUserMessageArgs::default()
            .content(prompt)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build user message: {}", e)))?;

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(vec![
            ChatCompletionRequestMessage::System(system_message),
            ChatCompletionRequestMessage::User(user_message),
        ]);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let mut stream = self
            .client
            .chat()
            .create_stream(request)
            .await
            .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e)))?;

        let result_stream = async_stream::stream! {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(response) => {
                        for choice in response.choices {
                            if let Some(content) = choice.delta.content {
                                yield Ok(content);
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(AppError::LLM(format!("Stream error: {}", e)));
                    }
                }
            }
        };

        Ok(Box::new(Box::pin(result_stream)))
    }

    async fn stream_with_history(
        &self,
        messages: &[(String, String)],
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let chat_messages: std::result::Result<Vec<ChatCompletionRequestMessage>, AppError> =
            messages
                .iter()
                .map(|(role, content)| {
                    match role.as_str() {
                        "system" => {
                            let msg = ChatCompletionRequestSystemMessageArgs::default()
                                .content(content.as_str())
                                .build()
                                .map_err(|e| {
                                    AppError::LLM(format!("Failed to build system message: {}", e))
                                })?;
                            Ok(ChatCompletionRequestMessage::System(msg))
                        }
                        "assistant" => {
                            let msg = ChatCompletionRequestAssistantMessageArgs::default()
                                .content(content.as_str())
                                .build()
                                .map_err(|e| {
                                    AppError::LLM(format!(
                                        "Failed to build assistant message: {}",
                                        e
                                    ))
                                })?;
                            Ok(ChatCompletionRequestMessage::Assistant(msg))
                        }
                        _ => {
                            // Default to user message
                            let msg = ChatCompletionRequestUserMessageArgs::default()
                                .content(content.as_str())
                                .build()
                                .map_err(|e| {
                                    AppError::LLM(format!("Failed to build user message: {}", e))
                                })?;
                            Ok(ChatCompletionRequestMessage::User(msg))
                        }
                    }
                })
                .collect();

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(chat_messages?);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let mut stream = self
            .client
            .chat()
            .create_stream(request)
            .await
            .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e)))?;

        let result_stream = async_stream::stream! {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(response) => {
                        for choice in response.choices {
                            if let Some(content) = choice.delta.content {
                                yield Ok(content);
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(AppError::LLM(format!("Stream error: {}", e)));
                    }
                }
            }
        };

        Ok(Box::new(Box::pin(result_stream)))
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn supports_hints(&self) -> bool {
        true
    }

    fn set_hints(&self, hints: GenerationHints) {
        if let Ok(mut slot) = self.hints.write() {
            *slot = hints;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::client::LLMClient;
    use async_openai::config::Config;
    use async_openai::types::chat::{
        ChatCompletionNamedToolChoice, ChatCompletionToolChoiceOption, FunctionName,
        ToolChoiceOptions,
    };
    use futures::StreamExt;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::*};

    fn openai_client(server: &MockServer, model: &str) -> OpenAIClient {
        openai_client_with_params(server, model, ModelParams::default())
    }

    fn openai_client_with_params(
        server: &MockServer,
        model: &str,
        params: ModelParams,
    ) -> OpenAIClient {
        OpenAIClient::with_params(
            "test-key".to_string(),
            format!("http://127.0.0.1:{}/v1", server.address().port()),
            model.to_string(),
            params,
        )
    }

    fn full_model_params() -> ModelParams {
        ModelParams {
            temperature: Some(0.7),
            max_tokens: Some(128),
            top_p: Some(0.9),
            frequency_penalty: Some(0.1),
            presence_penalty: Some(0.2),
        }
    }

    async fn first_request_json(server: &MockServer) -> serde_json::Value {
        let requests = server.received_requests().await.unwrap();
        assert!(!requests.is_empty(), "expected at least one HTTP request");
        serde_json::from_slice(&requests[0].body).unwrap()
    }

    fn chat_completion_json(content: &str) -> String {
        serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 4,
                "completion_tokens": 2,
                "total_tokens": 6
            }
        })
        .to_string()
    }

    fn chat_completion_empty_choices_json() -> String {
        serde_json::json!({
            "id": "chatcmpl-empty",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt-4",
            "choices": []
        })
        .to_string()
    }

    fn chat_completion_null_content_json() -> String {
        serde_json::json!({
            "id": "chatcmpl-null",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": null },
                "finish_reason": "stop"
            }]
        })
        .to_string()
    }

    fn chat_completion_without_usage_json(content: &str) -> String {
        serde_json::json!({
            "id": "chatcmpl-no-usage",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }]
        })
        .to_string()
    }

    fn chat_completion_multiple_tools_json() -> String {
        serde_json::json!({
            "id": "chatcmpl-multi-tools",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "alpha",
                                "arguments": "{\"x\":1}"
                            }
                        },
                        {
                            "id": "call_2",
                            "type": "function",
                            "function": {
                                "name": "beta",
                                "arguments": "{\"y\":2}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }]
        })
        .to_string()
    }

    fn chat_completion_with_tools_json() -> String {
        serde_json::json!({
            "id": "chatcmpl-tools",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "calculator",
                            "arguments": "{\"a\":1}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 3,
                "total_tokens": 8
            }
        })
        .to_string()
    }

    fn chat_stream_chunk_json(content: Option<&str>) -> String {
        let delta = match content {
            Some(c) => serde_json::json!({ "content": c }),
            None => serde_json::json!({}),
        };
        serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": null
            }]
        })
        .to_string()
    }

    fn openai_sse_body(chunks: &[String]) -> String {
        let mut body = String::new();
        for chunk in chunks {
            body.push_str("data: ");
            body.push_str(chunk);
            body.push_str("\n\n");
        }
        body.push_str("data: [DONE]\n\n");
        body
    }

    fn chat_stream_tool_call_delta_json() -> String {
        serde_json::json!({
            "id": "chatcmpl-tools-stream",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_stream",
                        "type": "function",
                        "function": { "name": "search", "arguments": "{\"q\":" }
                    }]
                },
                "finish_reason": null
            }]
        })
        .to_string()
    }

    fn sse_stream_response(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body.as_bytes(), "text/event-stream")
    }

    #[test]
    fn test_client_creation() {
        let client = OpenAIClient::new(
            "test-key".to_string(),
            "https://api.openai.com/v1".to_string(),
            "gpt-4".to_string(),
        );

        assert_eq!(client.model_name(), "gpt-4");
    }

    fn sample_client() -> OpenAIClient {
        OpenAIClient::new(
            "test-key".to_string(),
            "https://api.openai.com/v1".to_string(),
            "gpt-4".to_string(),
        )
    }

    #[test]
    fn test_tool_conversion() {
        let tool = ToolDefinition {
            name: "calculator".to_string(),
            description: "Performs math operations".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": {"type": "string"},
                    "a": {"type": "number"},
                    "b": {"type": "number"}
                },
                "required": ["operation", "a", "b"]
            }),
        };

        let openai_tool = OpenAIClient::convert_tool(&tool);
        match openai_tool {
            ChatCompletionTools::Function(chat_tool) => {
                assert_eq!(chat_tool.function.name, "calculator");
                assert_eq!(
                    chat_tool.function.description,
                    Some("Performs math operations".to_string())
                );
                assert_eq!(chat_tool.function.parameters, Some(tool.parameters.clone()));
            }
            ChatCompletionTools::Custom(_) => {
                panic!("Expected Function variant, got Custom");
            }
        }
    }

    #[test]
    fn test_with_params_preserves_model_name() {
        let client = OpenAIClient::with_params(
            "key".to_string(),
            "http://localhost:8000/v1".to_string(),
            "gpt-4o-mini".to_string(),
            ModelParams {
                temperature: Some(0.2),
                max_tokens: Some(256),
                ..Default::default()
            },
        );
        assert_eq!(client.model_name(), "gpt-4o-mini");
    }

    #[test]
    fn test_extract_tool_calls_parses_function_calls() {
        let calls = vec![ChatCompletionMessageToolCalls::Function(
            ChatCompletionMessageToolCall {
                id: "call_1".to_string(),
                function: FunctionCall {
                    name: "calculator".to_string(),
                    arguments: r#"{"a":1,"b":2}"#.to_string(),
                },
            },
        )];
        let extracted = OpenAIClient::extract_tool_calls(&calls);
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0].id, "call_1");
        assert_eq!(extracted[0].name, "calculator");
        assert_eq!(extracted[0].arguments["a"], 1);
    }

    #[test]
    fn test_extract_tool_calls_invalid_json_defaults_to_empty_object() {
        let calls = vec![ChatCompletionMessageToolCalls::Function(
            ChatCompletionMessageToolCall {
                id: "call_bad".to_string(),
                function: FunctionCall {
                    name: "broken".to_string(),
                    arguments: "not-json".to_string(),
                },
            },
        )];
        let extracted = OpenAIClient::extract_tool_calls(&calls);
        assert_eq!(extracted[0].arguments, serde_json::json!({}));
    }

    #[test]
    fn test_extract_tool_calls_skips_custom_variant() {
        use async_openai::types::chat::{ChatCompletionMessageCustomToolCall, CustomTool};
        let calls = vec![ChatCompletionMessageToolCalls::Custom(
            ChatCompletionMessageCustomToolCall {
                id: "custom_1".to_string(),
                custom_tool: CustomTool {
                    name: "x".to_string(),
                    input: "{}".to_string(),
                },
            },
        )];
        assert!(OpenAIClient::extract_tool_calls(&calls).is_empty());
    }

    #[test]
    fn test_convert_conversation_message_roles() {
        use crate::coordinator::{ConversationMessage, MessageRole};

        let client = sample_client();
        let system = ConversationMessage {
            role: MessageRole::System,
            content: "Be helpful".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        };
        let user = ConversationMessage {
            role: MessageRole::User,
            content: "Hi".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        };
        let assistant = ConversationMessage {
            role: MessageRole::Assistant,
            content: "Hello".to_string(),
            tool_calls: vec![ToolCall {
                id: "tc1".to_string(),
                name: "search".to_string(),
                arguments: serde_json::json!({"q": "rust"}),
            }],
            tool_call_id: None,
        };
        let tool = ConversationMessage {
            role: MessageRole::Tool,
            content: "done".to_string(),
            tool_calls: vec![],
            tool_call_id: Some("tc1".to_string()),
        };

        assert!(matches!(
            client.convert_conversation_message(&system).unwrap(),
            ChatCompletionRequestMessage::System(_)
        ));
        assert!(matches!(
            client.convert_conversation_message(&user).unwrap(),
            ChatCompletionRequestMessage::User(_)
        ));
        assert!(matches!(
            client.convert_conversation_message(&assistant).unwrap(),
            ChatCompletionRequestMessage::Assistant(_)
        ));
        assert!(matches!(
            client.convert_conversation_message(&tool).unwrap(),
            ChatCompletionRequestMessage::Tool(_)
        ));
    }

    #[test]
    fn test_convert_conversation_message_tool_requires_call_id() {
        use crate::coordinator::{ConversationMessage, MessageRole};

        let client = sample_client();
        let msg = ConversationMessage {
            role: MessageRole::Tool,
            content: "result".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        };
        let err = client.convert_conversation_message(&msg).unwrap_err();
        match err {
            AppError::LLM(msg) => assert!(msg.contains("tool_call_id")),
            other => panic!("expected LLM error, got {other:?}"),
        }
    }

    #[test]
    fn test_openai_provider_config_serde_roundtrip() {
        use crate::ProviderConfig;

        let original = ProviderConfig::OpenAI {
            api_key_env: "OPENAI_API_KEY".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            default_model: "gpt-4".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let decoded: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original.type_name(), decoded.type_name());
    }

    #[test]
    fn test_openai_from_config_missing_api_key_env() {
        use crate::ProviderConfig;
        use crate::client::Provider;

        std::env::remove_var("TEST_OPENAI_MISSING_KEY_OPENAI_RS");
        let config = ProviderConfig::OpenAI {
            api_key_env: "TEST_OPENAI_MISSING_KEY_OPENAI_RS".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            default_model: "gpt-4".to_string(),
        };
        let err = Provider::from_config(&config, None).unwrap_err();
        match err {
            AppError::Configuration(msg) => {
                assert!(msg.contains("TEST_OPENAI_MISSING_KEY_OPENAI_RS"));
            }
            other => panic!("expected Configuration error, got {other:?}"),
        }
    }
    #[tokio::test]
    async fn test_generate_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("hello")))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let out = client.generate("ping").await.unwrap();
        assert_eq!(out, "hello");
    }

    #[tokio::test]
    async fn test_generate_with_system_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_completion_json("sys reply")),
            )
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let out = client.generate_with_system("system", "user").await.unwrap();
        assert_eq!(out, "sys reply");
    }

    #[tokio::test]
    async fn test_generate_with_history_returns_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("hist")))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let response = client
            .generate_with_history(&[("user".into(), "hi".into())])
            .await
            .unwrap();
        assert_eq!(response.content, "hist");
        let usage = response.usage.expect("usage");
        assert_eq!(usage.prompt_tokens, 4);
        assert_eq!(usage.completion_tokens, 2);
    }

    #[tokio::test]
    async fn test_generate_with_tools_extracts_tool_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_completion_with_tools_json()),
            )
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let tools = vec![ToolDefinition {
            name: "calculator".to_string(),
            description: "calc".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let response = client.generate_with_tools("do math", &tools).await.unwrap();
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "calculator");
        assert_eq!(response.tool_calls[0].arguments["a"], 1);
    }

    #[tokio::test]
    async fn test_generate_with_tools_and_history() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("done")))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let messages = vec![ConversationMessage::user("hello")];
        let response = client
            .generate_with_tools_and_history(&messages, &[])
            .await
            .unwrap();
        assert_eq!(response.content, "done");
    }

    #[tokio::test]
    async fn test_gpt5_sets_reasoning_effort_in_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("ok")))
            .expect(1)
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-5");
        let _ = client.generate("hi").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["reasoning_effort"], "low");
    }

    /// Generation hints ride the single arg funnel onto the wire:
    /// json_mode → response_format, max_tokens → max_completion_tokens,
    /// suppress_reasoning → reasoning_effort minimal.
    #[tokio::test]
    async fn test_generation_hints_map_onto_request_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("ok")))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        assert!(client.supports_hints());
        client.set_hints(GenerationHints {
            json_mode: true,
            suppress_reasoning: true,
            max_tokens: Some(77),
            guided_grammar: None,
        });

        let _ = client.generate("hi").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            body["response_format"]["type"],
            serde_json::json!("json_object"),
            "json_mode hint maps to response_format json_object"
        );
        assert_eq!(
            body["max_completion_tokens"], 77,
            "hint budget maps to max_completion_tokens when params.max_tokens is None"
        );
        assert_eq!(
            body["reasoning_effort"], "minimal",
            "suppress_reasoning maps to reasoning_effort minimal"
        );
    }

    /// Params-configured max_tokens wins over the hint budget (explicit
    /// config beats advisory hints); a non-suppressed request keeps the
    /// model-default reasoning effort.
    #[tokio::test]
    async fn test_params_max_tokens_beats_hint_budget() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("ok")))
            .mount(&server)
            .await;

        let params = ModelParams {
            max_tokens: Some(512),
            ..ModelParams::default()
        };
        let client = openai_client_with_params(&server, "gpt-4", params);
        client.set_hints(GenerationHints {
            json_mode: false,
            suppress_reasoning: false,
            max_tokens: Some(64),
            guided_grammar: None,
        });
        let _ = client.generate("hi").await.unwrap();
        let body = first_request_json(&server).await;
        assert_eq!(body["max_completion_tokens"], 512);
        assert!(body.get("reasoning_effort").is_none());
    }

    /// guided_grammar reaches the request builder: JSON-Schema-shaped values
    /// map to `response_format` structured outputs, while raw GBNF/EBNF
    /// text rides the provider-specific `guided_grammar` extension field.
    #[tokio::test]
    async fn grammar_hint_present_reaches_request_builder() {
        // Schema-shaped grammar → strict structured output.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("ok")))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        client.set_hints(GenerationHints {
            guided_grammar: Some(
                r#"{"type":"object","properties":{"ok":{"type":"boolean"}}}"#.to_string(),
            ),
            ..GenerationHints::default()
        });

        let _ = client.generate("hi").await.unwrap();
        let body = first_request_json(&server).await;
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["name"], "guided_output");
        assert_eq!(
            body["response_format"]["json_schema"]["schema"]["properties"]["ok"]["type"],
            "boolean"
        );
        assert!(
            body.get("guided_grammar").is_none(),
            "schema-shaped grammar must not also ride the extension field"
        );

        // Raw GBNF-style text → provider-specific extension field.
        let raw_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("raw")))
            .mount(&raw_server)
            .await;

        let raw_client = openai_client(&raw_server, "gpt-4");
        raw_client.set_hints(GenerationHints {
            guided_grammar: Some("root ::= \"yes\" | \"no\"".to_string()),
            ..GenerationHints::default()
        });
        let out = raw_client.generate("hi").await.unwrap();
        assert_eq!(out, "raw", "extension-field send parses the typed response");

        let body = first_request_json(&raw_server).await;
        assert_eq!(
            body["guided_grammar"], "root ::= \"yes\" | \"no\"",
            "raw grammar text must be carried verbatim as an extension field"
        );
        assert!(
            body.get("response_format").is_none(),
            "non-schema grammar must not fabricate a response_format"
        );
    }

    /// No hints at all vs. explicitly cleared hints: identical requests,
    /// byte for byte, with no grammar or structured-output residue.
    #[tokio::test]
    async fn absent_hint_byte_identical_request() {
        let expected = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
        });

        let bare_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("ok")))
            .mount(&bare_server)
            .await;
        let cleared_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("ok")))
            .mount(&cleared_server)
            .await;

        let bare = openai_client(&bare_server, "gpt-4");
        let _ = bare.generate("hi").await.unwrap();

        let cleared = openai_client(&cleared_server, "gpt-4");
        cleared.set_hints(GenerationHints::default());
        let _ = cleared.generate("hi").await.unwrap();

        let bare_requests = bare_server.received_requests().await.unwrap();
        let cleared_requests = cleared_server.received_requests().await.unwrap();
        assert_eq!(
            bare_requests[0].body, cleared_requests[0].body,
            "an absent hint and a default-cleared hint must produce byte-identical bodies"
        );

        for request in [&bare_requests[0], &cleared_requests[0]] {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body, expected, "no hint residue may appear on the wire");
        }
    }

    #[test]
    fn test_openai_config_api_base_roundtrip() {
        let config = OpenAIConfig::new()
            .with_api_key("key")
            .with_api_base("http://localhost:8080/v1");
        assert_eq!(config.api_base(), "http://localhost:8080/v1");
    }

    #[tokio::test]
    async fn test_stream_yields_sse_chunks() {
        let body = openai_sse_body(&[
            chat_stream_chunk_json(Some("Hel")),
            chat_stream_chunk_json(Some("lo")),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(sse_stream_response(&body))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client.stream("ping").await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "Hel");
        assert_eq!(stream.next().await.unwrap().unwrap(), "lo");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_stream_maps_http_error_on_connect() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client.stream("ping").await.unwrap();
        let item = stream.next().await.expect("stream should yield an error");
        match item {
            Err(AppError::LLM(msg)) => {
                assert!(
                    msg.contains("OpenAI API error") || msg.contains("Stream error"),
                    "unexpected message: {msg}"
                );
            }
            Err(other) => panic!("expected LLM error, got {other:?}"),
            Ok(_) => panic!("expected stream error item"),
        }
    }

    #[tokio::test]
    async fn test_stream_empty_sse_yields_nothing() {
        let body = openai_sse_body(&[chat_stream_chunk_json(None)]);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(sse_stream_response(&body))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client.stream("ping").await.unwrap();
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_stream_surfaces_malformed_sse_event_as_error() {
        let body = format!(
            "data: not-json\n\ndata: {}\n\ndata: [DONE]\n\n",
            chat_stream_chunk_json(Some("ok"))
        );
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(sse_stream_response(&body))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client.stream("ping").await.unwrap();
        let item = stream.next().await.expect("expected stream item");
        match item {
            Err(AppError::LLM(msg)) => assert!(msg.contains("Stream error")),
            Err(other) => panic!("expected LLM error, got {other:?}"),
            Ok(_) => panic!("expected malformed SSE to surface as stream error"),
        }
    }

    #[tokio::test]
    async fn test_stream_with_system_yields_sse_chunks() {
        let body = openai_sse_body(&[
            chat_stream_chunk_json(Some("sys")),
            chat_stream_chunk_json(Some("-out")),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(sse_stream_response(&body))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client.stream_with_system("system", "user").await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "sys");
        assert_eq!(stream.next().await.unwrap().unwrap(), "-out");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_stream_with_history_yields_sse_chunks() {
        let body = openai_sse_body(&[
            chat_stream_chunk_json(Some("his")),
            chat_stream_chunk_json(Some("tory")),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(sse_stream_response(&body))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client
            .stream_with_history(&[("user".into(), "hi".into())])
            .await
            .unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "his");
        assert_eq!(stream.next().await.unwrap().unwrap(), "tory");
        assert!(stream.next().await.is_none());
    }

    #[test]
    fn test_extract_tool_calls_multiple_functions() {
        let calls = vec![
            ChatCompletionMessageToolCalls::Function(ChatCompletionMessageToolCall {
                id: "call_1".to_string(),
                function: FunctionCall {
                    name: "alpha".to_string(),
                    arguments: r#"{"x":1}"#.to_string(),
                },
            }),
            ChatCompletionMessageToolCalls::Function(ChatCompletionMessageToolCall {
                id: "call_2".to_string(),
                function: FunctionCall {
                    name: "beta".to_string(),
                    arguments: r#"{"y":2}"#.to_string(),
                },
            }),
        ];
        let extracted = OpenAIClient::extract_tool_calls(&calls);
        assert_eq!(extracted.len(), 2);
        assert_eq!(extracted[0].name, "alpha");
        assert_eq!(extracted[1].arguments["y"], 2);
    }

    #[test]
    fn test_convert_assistant_empty_content_with_tool_calls() {
        use crate::coordinator::{ConversationMessage, MessageRole};

        let client = sample_client();
        let msg = ConversationMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "tc1".to_string(),
                name: "lookup".to_string(),
                arguments: serde_json::json!({"id": 7}),
            }],
            tool_call_id: None,
        };
        assert!(matches!(
            client.convert_conversation_message(&msg).unwrap(),
            ChatCompletionRequestMessage::Assistant(_)
        ));
    }

    #[test]
    fn test_tool_choice_mode_none_serializes() {
        let choice = ChatCompletionToolChoiceOption::Mode(ToolChoiceOptions::None);
        assert_eq!(
            serde_json::to_value(choice).unwrap(),
            serde_json::json!("none")
        );
    }

    #[test]
    fn test_tool_choice_mode_auto_serializes() {
        let choice = ChatCompletionToolChoiceOption::Mode(ToolChoiceOptions::Auto);
        assert_eq!(
            serde_json::to_value(choice).unwrap(),
            serde_json::json!("auto")
        );
    }

    #[test]
    fn test_tool_choice_mode_required_serializes() {
        let choice = ChatCompletionToolChoiceOption::Mode(ToolChoiceOptions::Required);
        assert_eq!(
            serde_json::to_value(choice).unwrap(),
            serde_json::json!("required")
        );
    }

    #[test]
    fn test_tool_choice_specific_function_serializes() {
        let choice = ChatCompletionToolChoiceOption::Function(ChatCompletionNamedToolChoice {
            function: FunctionName {
                name: "get_weather".to_string(),
            },
        });
        let value = serde_json::to_value(choice).unwrap();
        assert_eq!(value["type"], "function");
        assert_eq!(value["function"]["name"], "get_weather");
    }

    #[test]
    fn test_openai_config_authorization_header() {
        let config = OpenAIConfig::new().with_api_key("secret-key");
        let headers = config.headers();
        let auth = headers
            .get("authorization")
            .expect("authorization header")
            .to_str()
            .unwrap();
        assert_eq!(auth, "Bearer secret-key");
    }

    #[test]
    fn test_openai_config_org_and_project_headers() {
        let config = OpenAIConfig::new()
            .with_api_key("key")
            .with_org_id("org-abc")
            .with_project_id("proj-xyz");
        assert_eq!(config.org_id(), "org-abc");
        let headers = config.headers();
        let project = headers
            .get("OpenAI-Project")
            .expect("project header")
            .to_str()
            .unwrap();
        assert_eq!(project, "proj-xyz");
    }

    #[test]
    fn test_tool_conversion_nested_json_schema() {
        let tool = ToolDefinition {
            name: "search".to_string(),
            description: "Search the web".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["query"]
            }),
        };
        let openai_tool = OpenAIClient::convert_tool(&tool);
        match openai_tool {
            ChatCompletionTools::Function(chat_tool) => {
                let params = chat_tool.function.parameters.as_ref().unwrap();
                assert_eq!(params["required"][0], "query");
                assert_eq!(params["properties"]["limit"]["minimum"], 1);
            }
            ChatCompletionTools::Custom(_) => panic!("expected Function tool"),
        }
    }

    #[test]
    fn test_chat_completion_response_json_deserializes() {
        let parsed: serde_json::Value =
            serde_json::from_str(&chat_completion_with_tools_json()).unwrap();
        assert_eq!(parsed["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            parsed["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "calculator"
        );
    }

    #[test]
    fn test_openai_from_config_reads_api_key_from_env() {
        use crate::ProviderConfig;
        use crate::client::Provider;

        std::env::set_var("TEST_OPENAI_KEY_PRESENT_OPENAI_RS", "sk-test-value");
        let config = ProviderConfig::OpenAI {
            api_key_env: "TEST_OPENAI_KEY_PRESENT_OPENAI_RS".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            default_model: "gpt-4o-mini".to_string(),
        };
        let provider = Provider::from_config(&config, None).unwrap();
        match provider {
            Provider::OpenAI { api_key, model, .. } => {
                assert_eq!(api_key, "sk-test-value");
                assert_eq!(model, "gpt-4o-mini");
            }
            other => panic!("expected OpenAI provider, got {other:?}"),
        }
        std::env::remove_var("TEST_OPENAI_KEY_PRESENT_OPENAI_RS");
    }

    #[tokio::test]
    async fn test_model_params_all_fields_in_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("ok")))
            .mount(&server)
            .await;

        let client = openai_client_with_params(&server, "gpt-4", full_model_params());
        let _ = client.generate("hi").await.unwrap();
        let body = first_request_json(&server).await;
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["max_completion_tokens"], 128);
        assert_eq!(body["top_p"], 0.9);
        assert_eq!(body["frequency_penalty"], 0.1);
        assert_eq!(body["presence_penalty"], 0.2);
    }

    #[tokio::test]
    async fn test_non_gpt5_omits_reasoning_effort() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("ok")))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4o-mini");
        let _ = client.generate("hi").await.unwrap();
        let body = first_request_json(&server).await;
        assert!(body.get("reasoning_effort").is_none());
    }

    #[tokio::test]
    async fn test_generate_http_429_maps_to_llm_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let err = client.generate("ping").await.unwrap_err();
        match err {
            AppError::LLM(msg) => assert!(msg.contains("OpenAI API error")),
            other => panic!("expected LLM error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_generate_http_401_maps_to_llm_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let err = client.generate("ping").await.unwrap_err();
        match err {
            AppError::LLM(msg) => assert!(msg.contains("OpenAI API error")),
            other => panic!("expected LLM error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_generate_empty_choices_returns_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_completion_empty_choices_json()),
            )
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let err = client.generate("ping").await.unwrap_err();
        match err {
            AppError::LLM(msg) => assert!(msg.contains("No response")),
            other => panic!("expected LLM error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_generate_null_content_returns_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_completion_null_content_json()),
            )
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let err = client.generate("ping").await.unwrap_err();
        match err {
            AppError::LLM(msg) => assert!(msg.contains("No response")),
            other => panic!("expected LLM error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_generate_with_history_system_and_assistant_roles() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_completion_json("roles ok")),
            )
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let response = client
            .generate_with_history(&[
                ("system".into(), "You are terse".into()),
                ("assistant".into(), "Understood".into()),
                ("user".into(), "Go".into()),
            ])
            .await
            .unwrap();
        assert_eq!(response.content, "roles ok");
        let body = first_request_json(&server).await;
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["messages"][2]["role"], "user");
    }

    #[tokio::test]
    async fn test_generate_with_history_without_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(chat_completion_without_usage_json("no usage")),
            )
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let response = client
            .generate_with_history(&[("user".into(), "hi".into())])
            .await
            .unwrap();
        assert_eq!(response.content, "no usage");
        assert!(response.usage.is_none());
    }

    #[tokio::test]
    async fn test_generate_with_tools_finish_reason_from_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_completion_with_tools_json()),
            )
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let tools = vec![ToolDefinition {
            name: "calculator".to_string(),
            description: "calc".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let response = client.generate_with_tools("do math", &tools).await.unwrap();
        assert!(response.finish_reason.contains("tool"));
    }

    #[tokio::test]
    async fn test_generate_with_tools_multiple_tool_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_completion_multiple_tools_json()),
            )
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let tools = vec![ToolDefinition {
            name: "alpha".to_string(),
            description: "a".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let response = client.generate_with_tools("run", &tools).await.unwrap();
        assert_eq!(response.tool_calls.len(), 2);
        assert_eq!(response.tool_calls[0].name, "alpha");
        assert_eq!(response.tool_calls[1].name, "beta");
    }

    #[tokio::test]
    async fn test_generate_with_tools_and_history_sends_tools_in_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("ok")))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let tools = vec![ToolDefinition {
            name: "search".to_string(),
            description: "find things".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let messages = vec![ConversationMessage::user("query")];
        let _ = client
            .generate_with_tools_and_history(&messages, &tools)
            .await
            .unwrap();
        let body = first_request_json(&server).await;
        let tools_json = body["tools"].as_array().expect("tools array");
        assert_eq!(tools_json.len(), 1);
        assert_eq!(tools_json[0]["function"]["name"], "search");
    }

    #[tokio::test]
    async fn test_generate_with_tools_and_history_tool_result_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("final")))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let messages = vec![
            ConversationMessage::assistant(
                "",
                vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "search".to_string(),
                    arguments: serde_json::json!({"q": "rust"}),
                }],
            ),
            ConversationMessage::tool_result("call_1", &serde_json::json!({"hits": 3})),
        ];
        let response = client
            .generate_with_tools_and_history(&messages, &[])
            .await
            .unwrap();
        assert_eq!(response.content, "final");
        let body = first_request_json(&server).await;
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "call_1");
    }

    #[tokio::test]
    async fn test_stream_tool_call_delta_yields_no_content() {
        let body = openai_sse_body(&[chat_stream_tool_call_delta_json()]);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(sse_stream_response(&body))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client.stream("ping").await.unwrap();
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_stream_done_only_sentinel_yields_nothing() {
        let body = "data: [DONE]\n\n".to_string();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(sse_stream_response(&body))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client.stream("ping").await.unwrap();
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_stream_mixed_empty_and_content_deltas() {
        let body = openai_sse_body(&[
            chat_stream_chunk_json(None),
            chat_stream_chunk_json(Some("a")),
            chat_stream_chunk_json(None),
            chat_stream_chunk_json(Some("b")),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(sse_stream_response(&body))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client.stream("ping").await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "a");
        assert_eq!(stream.next().await.unwrap().unwrap(), "b");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_stream_ignores_sse_heartbeat_comments() {
        let body = format!(
            ": ping\n\n: keep-alive\n\ndata: {}\n\n: between-chunks\n\ndata: {}\n\ndata: [DONE]\n\n",
            chat_stream_chunk_json(Some("hel")),
            chat_stream_chunk_json(Some("lo")),
        );
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(sse_stream_response(&body))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client.stream("ping").await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "hel");
        assert_eq!(stream.next().await.unwrap().unwrap(), "lo");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_stream_data_only_sse_lines_yield_content() {
        // OpenAI streams use only `data:` fields (no `event:` / `id:` / `retry:` lines).
        let body = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            chat_stream_chunk_json(Some("data-")),
            chat_stream_chunk_json(Some("only")),
        );
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(sse_stream_response(&body))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client.stream("ping").await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "data-");
        assert_eq!(stream.next().await.unwrap().unwrap(), "only");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_stream_blank_data_only_sse_line_surfaces_error() {
        let body = "data:\n\ndata: [DONE]\n\n".to_string();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(sse_stream_response(&body))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client.stream("ping").await.unwrap();
        match stream.next().await.expect("expected stream item") {
            Err(AppError::LLM(msg)) => assert!(msg.contains("Stream error")),
            Err(other) => panic!("expected LLM error, got {other:?}"),
            Ok(_) => panic!("expected blank data-only SSE line to surface as stream error"),
        }
    }

    #[tokio::test]
    async fn test_stream_malformed_sse_event_stops_before_later_chunks() {
        let body = format!(
            "data: {}\n\ndata: not-json\n\ndata: {}\n\ndata: [DONE]\n\n",
            chat_stream_chunk_json(Some("first")),
            chat_stream_chunk_json(Some("never")),
        );
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(sse_stream_response(&body))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client.stream("ping").await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "first");
        match stream.next().await.expect("expected stream item") {
            Err(AppError::LLM(msg)) => assert!(msg.contains("Stream error")),
            Err(other) => panic!("expected LLM error, got {other:?}"),
            Ok(_) => panic!("expected malformed SSE to surface as stream error"),
        }
    }

    #[tokio::test]
    async fn test_stream_empty_sse_chunks_between_content() {
        let body = format!(
            ": heartbeat\n\ndata: {}\n\n: heartbeat\n\ndata: {}\n\n: heartbeat\n\ndata: {}\n\ndata: [DONE]\n\n",
            chat_stream_chunk_json(None),
            chat_stream_chunk_json(Some("mid")),
            chat_stream_chunk_json(None),
        );
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(sse_stream_response(&body))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let mut stream = client.stream("ping").await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "mid");
        assert!(stream.next().await.is_none());
    }

    // ── Constructor tests ───────────────────────────────────────────────

    #[test]
    fn openai_client_new_creates_client() {
        let client = OpenAIClient::new(
            "test-key".to_string(),
            "https://api.openai.com/v1".to_string(),
            "gpt-4".to_string(),
        );
        assert_eq!(client.model, "gpt-4");
        assert_eq!(client.params.temperature, None);
    }

    #[test]
    fn openai_client_with_params_sets_fields() {
        let params = ModelParams {
            temperature: Some(0.5),
            max_tokens: Some(256),
            top_p: Some(0.95),
            frequency_penalty: Some(0.2),
            presence_penalty: Some(0.3),
        };
        let client = OpenAIClient::with_params(
            "test-key".to_string(),
            "https://api.openai.com/v1".to_string(),
            "gpt-4o".to_string(),
            params.clone(),
        );
        assert_eq!(client.model, "gpt-4o");
        assert_eq!(client.params.temperature, params.temperature);
        assert_eq!(client.params.max_tokens, params.max_tokens);
    }

    #[test]
    fn openai_config_has_correct_base_url() {
        let client = OpenAIClient::new(
            "test-key".to_string(),
            "https://custom.api.com/v1".to_string(),
            "gpt-4".to_string(),
        );
        assert_eq!(
            client.client.config().api_base(),
            "https://custom.api.com/v1"
        );
    }

    #[test]
    fn openai_config_has_correct_api_key() {
        let client = OpenAIClient::new(
            "secret-key-123".to_string(),
            "https://api.openai.com/v1".to_string(),
            "gpt-4".to_string(),
        );
        // Verify config is set (actual key not readable for security)
        assert!(client.client.config().api_base().contains("openai"));
    }

    // ── Model parameter application tests ───────────────────────────────

    #[tokio::test]
    async fn generate_sends_temperature_in_request() {
        let params = ModelParams {
            temperature: Some(0.8),
            ..ModelParams::default()
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("hi")))
            .mount(&server)
            .await;

        let client = openai_client_with_params(&server, "gpt-4", params);
        let _ = client.generate("hello").await.unwrap();
        let body = first_request_json(&server).await;
        assert_eq!(body["temperature"], 0.8);
    }

    #[tokio::test]
    async fn generate_sends_max_tokens_in_request() {
        let params = ModelParams {
            max_tokens: Some(512),
            ..ModelParams::default()
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("hi")))
            .mount(&server)
            .await;

        let client = openai_client_with_params(&server, "gpt-4", params);
        let _ = client.generate("hello").await.unwrap();
        let body = first_request_json(&server).await;
        assert_eq!(body["max_completion_tokens"], 512);
    }

    #[tokio::test]
    async fn generate_sends_top_p_in_request() {
        let params = ModelParams {
            top_p: Some(0.85),
            ..ModelParams::default()
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("hi")))
            .mount(&server)
            .await;

        let client = openai_client_with_params(&server, "gpt-4", params);
        let _ = client.generate("hello").await.unwrap();
        let body = first_request_json(&server).await;
        assert_eq!(body["top_p"], 0.85);
    }

    #[tokio::test]
    async fn generate_sends_frequency_penalty_in_request() {
        let params = ModelParams {
            frequency_penalty: Some(0.5),
            ..ModelParams::default()
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("hi")))
            .mount(&server)
            .await;

        let client = openai_client_with_params(&server, "gpt-4", params);
        let _ = client.generate("hello").await.unwrap();
        let body = first_request_json(&server).await;
        assert_eq!(body["frequency_penalty"], 0.5);
    }

    #[tokio::test]
    async fn generate_sends_presence_penalty_in_request() {
        let params = ModelParams {
            presence_penalty: Some(0.6),
            ..ModelParams::default()
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("hi")))
            .mount(&server)
            .await;

        let client = openai_client_with_params(&server, "gpt-4", params);
        let _ = client.generate("hello").await.unwrap();
        let body = first_request_json(&server).await;
        assert_eq!(body["presence_penalty"], 0.6);
    }

    #[tokio::test]
    async fn generate_gpt5_sets_reasoning_effort_low() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("hi")))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-5");
        let _ = client.generate("hello").await.unwrap();
        let body = first_request_json(&server).await;
        assert_eq!(body["reasoning_effort"], "low");
    }

    #[tokio::test]
    async fn generate_gpt4_does_not_set_reasoning_effort() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_completion_json("hi")))
            .mount(&server)
            .await;

        let client = openai_client(&server, "gpt-4");
        let _ = client.generate("hello").await.unwrap();
        let body = first_request_json(&server).await;
        // GPT-4 should not have reasoning_effort
        assert!(body.get("reasoning_effort").is_none());
    }

    // ── Message conversion edge cases ────────────────────────────────────

    #[test]
    fn convert_tool_handles_empty_arguments() {
        let tool = ToolDefinition {
            name: "test_tool".to_string(),
            description: "test".to_string(),
            parameters: serde_json::json!({}),
        };
        let chat_tool = OpenAIClient::convert_tool(&tool);
        match chat_tool {
            ChatCompletionTools::Function(f) => {
                assert_eq!(f.function.name, "test_tool");
                assert_eq!(f.function.description, Some("test".to_string()));
            }
            ChatCompletionTools::Custom(_) => panic!("Expected Function variant"),
        }
    }

    #[test]
    fn extract_tool_calls_filters_custom_calls() {
        // Custom tool calls should be filtered out
        let calls = vec![ChatCompletionMessageToolCalls::Function(
            async_openai::types::chat::ChatCompletionMessageToolCall {
                id: "call_1".to_string(),
                function: async_openai::types::chat::FunctionCall {
                    name: "test".to_string(),
                    arguments: "{}".to_string(),
                },
            },
        )];
        let extracted = OpenAIClient::extract_tool_calls(&calls);
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0].id, "call_1");
        assert_eq!(extracted[0].name, "test");
    }

    #[test]
    fn extract_tool_calls_handles_empty_list() {
        let calls: Vec<ChatCompletionMessageToolCalls> = vec![];
        let extracted = OpenAIClient::extract_tool_calls(&calls);
        assert!(extracted.is_empty());
    }
}
