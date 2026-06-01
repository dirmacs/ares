//! LlamaCpp LLM client implementation
//!
//! This module provides integration with llama.cpp via the `llama-cpp-2` crate
//! for direct GGUF model loading and local inference.
//!
//! # Features
//!
//! Enable with the `llamacpp` feature flag. For GPU acceleration:
//! - `llamacpp-cuda` - NVIDIA CUDA support
//! - `llamacpp-metal` - Apple Metal support
//! - `llamacpp-vulkan` - Vulkan support
//!
//! # Example
//!
//! ```rust,ignore
//! use ares::llm::{LLMClient, Provider};
//!
//! let provider = Provider::LlamaCpp {
//!     model_path: "/path/to/model.gguf".to_string(),
//! };
//! let client = provider.create_client().await?;
//! let response = client.generate("Hello, world!").await?;
//! ```

use crate::client::{LLMClient, LLMResponse, ModelParams};
use crate::coordinator::{ConversationMessage, MessageRole};
use ares_types::types::{AppError, Result, ToolDefinition};
use async_stream::stream;
use async_trait::async_trait;
use futures::Stream;
use llama_cpp_2::{
    context::params::LlamaContextParams,
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{params::LlamaModelParams, AddBos, LlamaModel, Special},
    sampling::LlamaSampler,
};
use std::num::NonZeroU32;
use std::sync::Arc;
use tokio::sync::mpsc;

/// LlamaCpp client for local GGUF model inference
#[derive(Debug)]
pub struct LlamaCppClient {
    model_path: String,
    model: Arc<LlamaModel>,
    backend: Arc<LlamaBackend>,
    /// Context size for the model
    n_ctx: u32,
    /// Number of threads to use
    n_threads: i32,
    /// Maximum tokens to generate
    max_tokens: u32,
    /// Temperature for sampling
    temperature: f32,
    /// Top-p (nucleus sampling) parameter
    top_p: f32,
}

impl LlamaCppClient {
    /// Create a new LlamaCpp client
    ///
    /// # Arguments
    ///
    /// * `model_path` - Path to a GGUF model file
    ///
    /// # Errors
    ///
    /// Returns an error if the model file doesn't exist or can't be loaded.
    pub fn new(model_path: String) -> Result<Self> {
        Self::with_config_params(model_path, 4096, 4, 512, 0.7, 0.9)
    }

    /// Create a new LlamaCpp client with ModelParams
    ///
    /// # Arguments
    ///
    /// * `model_path` - Path to a GGUF model file
    /// * `params` - Model inference parameters
    pub fn with_params(model_path: String, params: ModelParams) -> Result<Self> {
        Self::with_config_params(
            model_path,
            4096,
            4,
            params.max_tokens.unwrap_or(512),
            params.temperature.unwrap_or(0.7),
            params.top_p.unwrap_or(0.9),
        )
    }

    /// Create a new LlamaCpp client with all configurable parameters
    ///
    /// # Arguments
    ///
    /// * `model_path` - Path to a GGUF model file
    /// * `n_ctx` - Context size (default: 4096)
    /// * `n_threads` - Number of CPU threads (default: 4)
    /// * `max_tokens` - Maximum tokens to generate (default: 512)
    /// * `temperature` - Sampling temperature (default: 0.7)
    /// * `top_p` - Nucleus sampling parameter (default: 0.9)
    pub fn with_config_params(
        model_path: String,
        n_ctx: u32,
        n_threads: i32,
        max_tokens: u32,
        temperature: f32,
        top_p: f32,
    ) -> Result<Self> {
        // Initialize the backend (must be done once)
        let backend = LlamaBackend::init()
            .map_err(|e| AppError::LLM(format!("Failed to initialize llama backend: {}", e)))?;

        if !std::path::Path::new(&model_path).is_file() {
            return Err(AppError::LLM(format!(
                "Failed to load model from '{}': model file not found",
                model_path
            )));
        }

        // Set up model parameters
        let model_params = LlamaModelParams::default();

        // Load the model
        let model =
            LlamaModel::load_from_file(&backend, &model_path, &model_params).map_err(|e| {
                AppError::LLM(format!("Failed to load model from '{}': {}", model_path, e))
            })?;

        Ok(Self {
            model_path,
            model: Arc::new(model),
            backend: Arc::new(backend),
            n_ctx,
            n_threads,
            max_tokens,
            temperature,
            top_p,
        })
    }

    /// Get the model path
    pub fn model_path(&self) -> &str {
        &self.model_path
    }

    /// Get the backend reference (needed for context creation)
    pub fn backend(&self) -> &LlamaBackend {
        &self.backend
    }

    /// Get the configured max tokens
    pub fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    /// Set max tokens for generation
    pub fn set_max_tokens(&mut self, max_tokens: u32) {
        self.max_tokens = max_tokens;
    }

    /// Generate text from tokens (internal implementation)
    async fn generate_internal(&self, prompt: &str, max_tokens: u32) -> Result<String> {
        let model = self.model.clone();
        let backend = self.backend.clone();
        let n_ctx = self.n_ctx;
        let n_threads = self.n_threads;
        let temperature = self.temperature;
        let top_p = self.top_p;
        let prompt = prompt.to_string();

        // Run blocking llama operations in a spawn_blocking task
        tokio::task::spawn_blocking(move || {
            Self::generate_sync(
                &model,
                &backend,
                n_ctx,
                n_threads,
                &prompt,
                max_tokens,
                temperature,
                top_p,
            )
        })
        .await
        .map_err(|e| AppError::LLM(format!("Task join error: {}", e)))?
    }

    /// Synchronous generation (runs in spawn_blocking)
    fn generate_sync(
        model: &LlamaModel,
        backend: &LlamaBackend,
        n_ctx: u32,
        n_threads: i32,
        prompt: &str,
        max_tokens: u32,
        temperature: f32,
        top_p: f32,
    ) -> Result<String> {
        // Create context parameters
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(n_ctx))
            .with_n_threads(n_threads)
            .with_n_threads_batch(n_threads);

        // Create context - pass backend reference
        let mut ctx = model
            .new_context(backend, ctx_params)
            .map_err(|e| AppError::LLM(format!("Failed to create context: {}", e)))?;

        // Tokenize the prompt
        let tokens = model
            .str_to_token(prompt, AddBos::Always)
            .map_err(|e| AppError::LLM(format!("Failed to tokenize prompt: {}", e)))?;

        if tokens.is_empty() {
            return Err(AppError::LLM("Empty prompt after tokenization".to_string()));
        }

        // Create a batch for the tokens
        let mut batch = LlamaBatch::new(n_ctx as usize, 1);

        // Add tokens to batch
        for (i, token) in tokens.iter().enumerate() {
            let is_last = i == tokens.len() - 1;
            batch
                .add(*token, i as i32, &[0], is_last)
                .map_err(|e| AppError::LLM(format!("Failed to add token to batch: {}", e)))?;
        }

        // Decode the batch (process input tokens)
        ctx.decode(&mut batch)
            .map_err(|e| AppError::LLM(format!("Failed to decode batch: {}", e)))?;

        // Set up sampler for generation with configured parameters
        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::temp(temperature),
            LlamaSampler::top_p(top_p, 1),
            LlamaSampler::dist(42),
        ]);

        // Generate tokens
        let mut output_tokens = Vec::new();
        let mut n_cur = tokens.len();

        for _ in 0..max_tokens {
            // Sample the next token
            let new_token = sampler.sample(&ctx, -1);

            // Check for end of generation
            if model.is_eog_token(new_token) {
                break;
            }

            output_tokens.push(new_token);

            // Prepare batch for next token
            batch.clear();
            batch
                .add(new_token, n_cur as i32, &[0], true)
                .map_err(|e| {
                    AppError::LLM(format!("Failed to add generated token to batch: {}", e))
                })?;

            // Decode the new token
            ctx.decode(&mut batch)
                .map_err(|e| AppError::LLM(format!("Failed to decode generated token: {}", e)))?;

            n_cur += 1;
        }

        // Convert all tokens to string
        let mut result = String::new();
        for token in &output_tokens {
            // Dereference the token to get LlamaToken value
            if let Ok(piece) = model.token_to_str_with_size(*token, 256, Special::Tokenize) {
                result.push_str(&piece);
            }
        }

        Ok(result)
    }

    /// Streaming generation using channel-based approach
    async fn stream_internal(
        &self,
        prompt: &str,
        max_tokens: u32,
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let model = self.model.clone();
        let backend = self.backend.clone();
        let n_ctx = self.n_ctx;
        let n_threads = self.n_threads;
        let temperature = self.temperature;
        let top_p = self.top_p;
        let prompt = prompt.to_string();

        // Create a channel for streaming tokens
        let (tx, mut rx) = mpsc::channel::<Result<String>>(32);

        // Spawn the blocking generation task
        tokio::task::spawn_blocking(move || {
            let result = Self::stream_sync(
                &model,
                &backend,
                n_ctx,
                n_threads,
                &prompt,
                max_tokens,
                temperature,
                top_p,
                tx.clone(),
            );
            if let Err(e) = result {
                // Send error through channel if generation fails
                let _ = tx.blocking_send(Err(e));
            }
        });

        // Create an async stream from the receiver
        let output_stream = stream! {
            while let Some(chunk) = rx.recv().await {
                yield chunk;
            }
        };

        Ok(Box::new(Box::pin(output_stream)))
    }

    /// Synchronous streaming generation (sends tokens through channel)
    fn stream_sync(
        model: &LlamaModel,
        backend: &LlamaBackend,
        n_ctx: u32,
        n_threads: i32,
        prompt: &str,
        max_tokens: u32,
        temperature: f32,
        top_p: f32,
        tx: mpsc::Sender<Result<String>>,
    ) -> Result<()> {
        // Create context parameters
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(n_ctx))
            .with_n_threads(n_threads)
            .with_n_threads_batch(n_threads);

        // Create context
        let mut ctx = model
            .new_context(backend, ctx_params)
            .map_err(|e| AppError::LLM(format!("Failed to create context: {}", e)))?;

        // Tokenize the prompt
        let tokens = model
            .str_to_token(prompt, AddBos::Always)
            .map_err(|e| AppError::LLM(format!("Failed to tokenize prompt: {}", e)))?;

        if tokens.is_empty() {
            return Err(AppError::LLM("Empty prompt after tokenization".to_string()));
        }

        // Create a batch for the tokens
        let mut batch = LlamaBatch::new(n_ctx as usize, 1);

        // Add tokens to batch
        for (i, token) in tokens.iter().enumerate() {
            let is_last = i == tokens.len() - 1;
            batch
                .add(*token, i as i32, &[0], is_last)
                .map_err(|e| AppError::LLM(format!("Failed to add token to batch: {}", e)))?;
        }

        // Decode the batch (process input tokens)
        ctx.decode(&mut batch)
            .map_err(|e| AppError::LLM(format!("Failed to decode batch: {}", e)))?;

        // Set up sampler for generation with configured parameters
        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::temp(temperature),
            LlamaSampler::top_p(top_p, 1),
            LlamaSampler::dist(42),
        ]);

        // Generate and stream tokens
        let mut n_cur = tokens.len();

        for _ in 0..max_tokens {
            // Sample the next token
            let new_token = sampler.sample(&ctx, -1);

            // Check for end of generation
            if model.is_eog_token(new_token) {
                break;
            }

            // Convert token to string and send through channel
            if let Ok(piece) = model.token_to_str_with_size(new_token, 256, Special::Tokenize) {
                if !piece.is_empty() {
                    // If receiver is dropped, stop generation
                    if tx.blocking_send(Ok(piece)).is_err() {
                        break;
                    }
                }
            }

            // Prepare batch for next token
            batch.clear();
            batch
                .add(new_token, n_cur as i32, &[0], true)
                .map_err(|e| {
                    AppError::LLM(format!("Failed to add generated token to batch: {}", e))
                })?;

            // Decode the new token
            ctx.decode(&mut batch)
                .map_err(|e| AppError::LLM(format!("Failed to decode generated token: {}", e)))?;

            n_cur += 1;
        }

        Ok(())
    }

    /// Format messages into a prompt string (ChatML format)
    fn format_prompt(&self, system: Option<&str>, user: &str) -> String {
        format_chatml_prompt(system, user)
    }

    /// Format chat history into a prompt string
    fn format_history(&self, messages: &[(String, String)]) -> String {
        format_chatml_history(messages)
    }
}

/// Format a single-turn ChatML prompt.
fn format_chatml_prompt(system: Option<&str>, user: &str) -> String {
    match system {
        Some(sys) => format!(
            "<|im_start|>system\n{}<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n",
            sys, user
        ),
        None => format!(
            "<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n",
            user
        ),
    }
}

/// Format multi-turn chat history into a ChatML prompt ending with an assistant turn.
fn format_chatml_history(messages: &[(String, String)]) -> String {
    let mut prompt = String::new();
    for (role, content) in messages {
        match role.as_str() {
            "system" => {
                prompt.push_str(&format!("<|im_start|>system\n{}<|im_end|>\n", content))
            }
            "user" => prompt.push_str(&format!("<|im_start|>user\n{}<|im_end|>\n", content)),
            "assistant" => {
                prompt.push_str(&format!(
                    "<|im_start|>assistant\n{}<|im_end|>\n",
                    content
                ))
            }
            _ => prompt.push_str(&format!("<|im_start|>user\n{}<|im_end|>\n", content)),
        }
    }
    prompt.push_str("<|im_start|>assistant\n");
    prompt
}

/// Parse tool calls from model output that uses the JSON `tool_call` envelope.
fn parse_tool_calls_from_content(content: &str) -> Vec<ares_types::types::ToolCall> {
    if !content.contains("\"tool_call\"") {
        return vec![];
    }
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) else {
        return vec![];
    };
    let Some(tool_call) = parsed.get("tool_call") else {
        return vec![];
    };
    vec![ares_types::types::ToolCall {
        id: uuid::Uuid::new_v4().to_string(),
        name: tool_call
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string(),
        arguments: tool_call
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({})),
    }]
}

/// Build the system prompt used by [`LLMClient::generate_with_tools`].
fn build_tools_system_prompt_generate(tools: &[ToolDefinition]) -> Result<String> {
    let tools_json = serde_json::to_string_pretty(tools)
        .map_err(|e| AppError::LLM(format!("Failed to serialize tools: {}", e)))?;
    Ok(format!(
        r#"You are a helpful assistant with access to the following tools:

{}

When you need to use a tool, respond ONLY with a JSON object in this exact format:
{{"tool_call": {{"name": "tool_name", "arguments": {{...}}}}}}

Otherwise, respond normally with text."#,
        tools_json
    ))
}

/// Build the optional tools preamble for [`LLMClient::generate_with_tools_and_history`].
fn build_tools_system_prompt_history(tools: &[ToolDefinition]) -> Result<Option<String>> {
    if tools.is_empty() {
        return Ok(None);
    }
    let tools_json = serde_json::to_string_pretty(tools)
        .map_err(|e| AppError::LLM(format!("Failed to serialize tools: {}", e)))?;
    Ok(Some(format!(
        r#"You have access to the following tools:

{}

When you need to use a tool, respond ONLY with a JSON object in this exact format:
{{"tool_call": {{"name": "tool_name", "arguments": {{...}}}}}}

Otherwise, respond normally with text."#,
        tools_json
    )))
}

/// Map coordinator messages into ChatML history pairs, optionally prefixing a tools system prompt.
fn conversation_messages_to_history(
    messages: &[ConversationMessage],
    tools_system: Option<&str>,
) -> Vec<(String, String)> {
    let mut history: Vec<(String, String)> = Vec::new();

    if let Some(system) = tools_system.filter(|s| !s.is_empty()) {
        history.push(("system".to_string(), system.to_string()));
    }

    for msg in messages {
        let role = match msg.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "user",
        };

        let content = if msg.role == MessageRole::Tool {
            format!(
                "[Tool Result{}]: {}",
                msg.tool_call_id
                    .as_ref()
                    .map(|id| format!(" for {}", id))
                    .unwrap_or_default(),
                msg.content
            )
        } else {
            msg.content.clone()
        };

        history.push((role.to_string(), content));
    }

    history
}

/// Derive OpenAI-style finish reasons from parsed tool calls.
fn finish_reason_from_tool_calls(
    tool_calls: &[ares_types::types::ToolCall],
) -> &'static str {
    if tool_calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    }
}


#[async_trait]
impl LLMClient for LlamaCppClient {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let formatted = self.format_prompt(None, prompt);
        self.generate_internal(&formatted, self.max_tokens).await
    }

    async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        let formatted = self.format_prompt(Some(system), prompt);
        self.generate_internal(&formatted, self.max_tokens).await
    }

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<LLMResponse> {
        let formatted = self.format_history(messages);
        let content = self.generate_internal(&formatted, self.max_tokens).await?;
        Ok(LLMResponse {
            content,
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
            // Note: llama-cpp-2 crate doesn't expose token counts in its API
            usage: None,
        })
    }

    async fn generate_with_tools(
        &self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        // For tool calling, we format the tools as part of the system prompt
        // and ask the model to respond in JSON format when it wants to call a tool
        let system = build_tools_system_prompt_generate(tools)?;

        let formatted = self.format_prompt(Some(&system), prompt);
        let content = self.generate_internal(&formatted, self.max_tokens).await?;

        let tool_calls = parse_tool_calls_from_content(&content);

        let finish_reason = finish_reason_from_tool_calls(&tool_calls);
        // Note: llama-cpp-2 crate doesn't expose token counts in its API
        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason: finish_reason.to_string(),
            usage: None,
        })
    }

    async fn generate_with_tools_and_history(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        let tools_system = build_tools_system_prompt_history(tools)?;
        let history = conversation_messages_to_history(messages, tools_system.as_deref());

        // Format and generate
        let formatted = self.format_history(&history);
        let content = self.generate_internal(&formatted, self.max_tokens).await?;

        let tool_calls = parse_tool_calls_from_content(&content);

        let finish_reason = finish_reason_from_tool_calls(&tool_calls);
        // Note: llama-cpp-2 crate doesn't expose token counts in its API
        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason: finish_reason.to_string(),
            usage: None,
        })
    }

    async fn stream(
        &self,
        prompt: &str,
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let formatted = self.format_prompt(None, prompt);
        self.stream_internal(&formatted, self.max_tokens).await
    }

    async fn stream_with_system(
        &self,
        system: &str,
        prompt: &str,
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let formatted = self.format_prompt(Some(system), prompt);
        self.stream_internal(&formatted, self.max_tokens).await
    }

    async fn stream_with_history(
        &self,
        messages: &[(String, String)],
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let formatted = self.format_history(messages);
        self.stream_internal(&formatted, self.max_tokens).await
    }

    fn model_name(&self) -> &str {
        &self.model_path
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_tools_system_prompt_generate, build_tools_system_prompt_history,
        conversation_messages_to_history, finish_reason_from_tool_calls,
        format_chatml_history, format_chatml_prompt, parse_tool_calls_from_content,
        LlamaCppClient,
    };
    use crate::client::ModelParams;
    use crate::coordinator::ConversationMessage;
    use ares_types::types::ToolDefinition;

    const USER_SUFFIX: &str = "<|im_start|>user\n";
    const ASSISTANT_SUFFIX: &str = "<|im_start|>assistant\n";

    #[test]
    fn test_format_chatml_prompt_without_system() {
        let formatted = format_chatml_prompt(None, "Hello");
        assert!(formatted.contains("Hello"));
        assert!(formatted.starts_with(USER_SUFFIX) || formatted.contains("\nHello"));
        assert!(formatted.ends_with(ASSISTANT_SUFFIX));
    }

    #[test]
    fn test_format_chatml_prompt_with_system() {
        let formatted = format_chatml_prompt(Some("You are helpful"), "Hello");
        assert!(formatted.contains("<|im_start|>system\nYou are helpful"));
        assert!(formatted.contains("<|im_start|>user\nHello"));
        assert!(formatted.ends_with(ASSISTANT_SUFFIX));
    }

    #[test]
    fn test_format_chatml_prompt_with_empty_user() {
        let formatted = format_chatml_prompt(Some("sys"), "");
        assert!(formatted.contains("<|im_start|>system\nsys"));
        assert!(formatted.ends_with(ASSISTANT_SUFFIX));
    }

    #[test]
    fn test_format_chatml_history_roles() {
        let history = vec![
            ("system".to_string(), "Be helpful".to_string()),
            ("user".to_string(), "Hello".to_string()),
            ("assistant".to_string(), "Hi!".to_string()),
            ("user".to_string(), "How are you?".to_string()),
        ];
        let result = format_chatml_history(&history);
        assert!(result.contains("Be helpful"));
        assert!(result.contains("Hello"));
        assert!(result.contains("Hi!"));
        assert!(result.contains("How are you?"));
        assert!(result.ends_with(ASSISTANT_SUFFIX));
    }

    #[test]
    fn test_format_chatml_history_empty() {
        assert_eq!(format_chatml_history(&[]), ASSISTANT_SUFFIX);
    }

    #[test]
    fn test_format_chatml_history_unknown_role_treated_as_user() {
        let history = vec![("unknown_role".to_string(), "fallback".to_string())];
        let result = format_chatml_history(&history);
        assert!(result.contains("<|im_start|>user\nfallback"));
    }

    #[test]
    fn test_format_chatml_history_preserves_message_order() {
        let history = vec![
            ("user".to_string(), "first".to_string()),
            ("assistant".to_string(), "second".to_string()),
        ];
        let result = format_chatml_history(&history);
        let first = result.find("first").expect("first");
        let second = result.find("second").expect("second");
        assert!(first < second);
    }

    #[test]
    fn test_parse_tool_calls_from_valid_json() {
        let response = r#"{"tool_call": {"name": "calculator", "arguments": {"a": 1, "b": 2}}}"#;
        let calls = parse_tool_calls_from_content(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "calculator");
        assert_eq!(calls[0].arguments["a"], 1);
        assert_eq!(calls[0].arguments["b"], 2);
        assert!(!calls[0].id.is_empty());
    }

    #[test]
    fn test_parse_tool_calls_ignores_plain_text() {
        assert!(parse_tool_calls_from_content("Just a normal response").is_empty());
    }

    #[test]
    fn test_parse_tool_calls_invalid_json_returns_empty() {
        let response = r#"{"tool_call": not-json}"#;
        assert!(parse_tool_calls_from_content(response).is_empty());
    }

    #[test]
    fn test_parse_tool_calls_missing_name_defaults_empty() {
        let response = r#"{"tool_call": {"arguments": {"x": 1}}}"#;
        let calls = parse_tool_calls_from_content(response);
        assert_eq!(calls.len(), 1);
        assert!(calls[0].name.is_empty());
        assert_eq!(calls[0].arguments["x"], 1);
    }

    #[test]
    fn test_parse_tool_calls_missing_arguments_defaults_to_empty_object() {
        let response = r#"{"tool_call": {"name": "lookup"}}"#;
        let calls = parse_tool_calls_from_content(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "lookup");
        assert_eq!(calls[0].arguments, serde_json::json!({}));
    }

    #[test]
    fn test_parse_tool_calls_valid_json_without_tool_call_key() {
        assert!(parse_tool_calls_from_content(r#"{"name": "x"}"#).is_empty());
    }

    #[test]
    fn test_parse_tool_calls_ignores_substring_in_prose() {
        let response = "The docs mention \"tool_call\" but this is not JSON";
        assert!(parse_tool_calls_from_content(response).is_empty());
    }

    #[test]
    fn test_build_tools_system_prompt_generate_includes_tool_name() {
        let tools = vec![ToolDefinition {
            name: "search".to_string(),
            description: "Search the web".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let prompt = build_tools_system_prompt_generate(&tools).unwrap();
        assert!(prompt.contains("search"));
        assert!(prompt.contains("tool_call"));
        assert!(prompt.starts_with("You are a helpful assistant"));
    }

    #[test]
    fn test_build_tools_system_prompt_history_none_when_empty() {
        assert_eq!(build_tools_system_prompt_history(&[]).unwrap(), None);
    }

    #[test]
    fn test_build_tools_system_prompt_history_some_when_tools_present() {
        let tools = vec![ToolDefinition {
            name: "calc".to_string(),
            description: "Math".to_string(),
            parameters: serde_json::json!({}),
        }];
        let prompt = build_tools_system_prompt_history(&tools).unwrap();
        assert!(prompt.as_ref().unwrap().contains("calc"));
    }

    #[test]
    fn test_conversation_messages_to_history_maps_roles() {
        let messages = vec![
            ConversationMessage::system("sys"),
            ConversationMessage::user("hi"),
            ConversationMessage::assistant("hello", vec![]),
        ];
        let history = conversation_messages_to_history(&messages, None);
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].0, "system");
        assert_eq!(history[1].0, "user");
        assert_eq!(history[2].0, "assistant");
    }

    #[test]
    fn test_conversation_messages_to_history_tool_result_with_id() {
        let messages = vec![ConversationMessage::tool_result("call-1", &serde_json::json!({"ok": true}))];
        let history = conversation_messages_to_history(&messages, None);
        assert_eq!(history[0].0, "user");
        assert!(history[0].1.contains("[Tool Result for call-1]"));
        assert!(history[0].1.contains("ok"));
    }

    #[test]
    fn test_conversation_messages_to_history_tool_result_without_id() {
        let messages = vec![ConversationMessage::tool_result("call-1", &serde_json::json!("done"))];
        let mut messages = messages;
        messages[0].tool_call_id = None;
        let history = conversation_messages_to_history(&messages, None);
        assert!(history[0].1.starts_with("[Tool Result]: "));
    }

    #[test]
    fn test_conversation_messages_to_history_prepends_tools_system() {
        let messages = vec![ConversationMessage::user("question")];
        let history = conversation_messages_to_history(&messages, Some("tools preamble"));
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].0, "system");
        assert_eq!(history[0].1, "tools preamble");
    }

    #[test]
    fn test_finish_reason_from_tool_calls() {
        assert_eq!(finish_reason_from_tool_calls(&[]), "stop");
        let calls = parse_tool_calls_from_content(
            r#"{"tool_call": {"name": "x", "arguments": {}}}"#,
        );
        assert_eq!(finish_reason_from_tool_calls(&calls), "tool_calls");
    }

    #[test]
    fn test_model_params_default_optional_fields() {
        let params = ModelParams::default();
        assert!(params.temperature.is_none());
        assert!(params.top_p.is_none());
        assert!(params.max_tokens.is_none());
    }

    #[test]
    fn test_llamacpp_provider_config_serde_roundtrip() {
        use ares_config::toml_config::ProviderConfig;

        let original = ProviderConfig::LlamaCpp {
            model_path: "/models/test.gguf".to_string(),
            n_ctx: 4096,
            n_threads: 4,
            max_tokens: 512,
        };
        let json = serde_json::to_string(&original).unwrap();
        let decoded: ProviderConfig = serde_json::from_str(&json).unwrap();
        match decoded {
            ProviderConfig::LlamaCpp {
                model_path,
                n_ctx,
                n_threads,
                max_tokens,
            } => {
                assert_eq!(model_path, "/models/test.gguf");
                assert_eq!(n_ctx, 4096);
                assert_eq!(n_threads, 4);
                assert_eq!(max_tokens, 512);
            }
            other => panic!("expected llamacpp variant, got {other:?}"),
        }
    }

    #[test]
    fn test_llamacpp_provider_config_json_tagged_roundtrip() {
        use ares_config::toml_config::ProviderConfig;

        let json = r#"{"type":"llamacpp","model_path":"/models/llama.gguf","n_ctx":8192,"n_threads":8,"max_tokens":256}"#;
        let decoded: ProviderConfig = serde_json::from_str(json).unwrap();
        assert_eq!(decoded.type_name(), "llamacpp");
        if let ProviderConfig::LlamaCpp { model_path, n_ctx, .. } = decoded {
            assert_eq!(model_path, "/models/llama.gguf");
            assert_eq!(n_ctx, 8192);
        } else {
            panic!("expected llamacpp variant");
        }
    }

    #[cfg(feature = "llamacpp")]
    #[test]
    fn test_llamacpp_client_creation_fails_with_invalid_path() {
        let model_path = "nonexistent_model.gguf".to_string();
        let result = LlamaCppClient::new(model_path.clone());
        assert!(result.is_err());
        let error = result.unwrap_err();
        match error {
            ares_types::types::AppError::LLM(msg) => {
                assert!(msg.contains("Failed to load model"));
                assert!(msg.contains(&model_path));
            }
            _ => panic!("Expected LLM error"),
        }
    }

    #[cfg(feature = "llamacpp")]
    #[test]
    fn test_llamacpp_client_with_params_uses_defaults_on_invalid_path() {
        let params = ModelParams {
            max_tokens: Some(256),
            temperature: Some(0.5),
            top_p: Some(0.8),
            ..Default::default()
        };
        let result = LlamaCppClient::with_params("dummy.gguf".to_string(), params);
        assert!(result.is_err());
        match result.unwrap_err() {
            ares_types::types::AppError::LLM(msg) => assert!(msg.contains("Failed to load model")),
            other => panic!("Expected LLM error, got {other:?}"),
        }
    }

    #[test]
    fn test_tool_history_formats_into_chatml() {
        let messages = vec![
            ConversationMessage::user("run tool"),
            ConversationMessage::tool_result("id-1", &serde_json::json!({"n": 1})),
        ];
        let history = conversation_messages_to_history(&messages, None);
        let prompt = format_chatml_history(&history);
        assert!(prompt.contains("run tool"));
        assert!(prompt.contains("[Tool Result for id-1]"));
    }
}

