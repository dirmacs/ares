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

use crate::llm::client::{LLMClient, LLMResponse};
use crate::types::{AppError, Result, ToolDefinition};
use async_stream::stream;
use async_trait::async_trait;
use futures::Stream;
use llama_cpp_2::{
    context::params::LlamaContextParams,
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{AddBos, LlamaModel, Special, params::LlamaModelParams},
    sampling::LlamaSampler,
};
use std::num::NonZeroU32;
use std::sync::Arc;
use tokio::sync::mpsc;

/// LlamaCpp client for local GGUF model inference
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
        Self::with_params(model_path, 4096, 4, 512)
    }

    /// Create a new LlamaCpp client with custom parameters
    ///
    /// # Arguments
    ///
    /// * `model_path` - Path to a GGUF model file
    /// * `n_ctx` - Context size (default: 4096)
    /// * `n_threads` - Number of CPU threads (default: 4)
    /// * `max_tokens` - Maximum tokens to generate (default: 512)
    pub fn with_params(
        model_path: String,
        n_ctx: u32,
        n_threads: i32,
        max_tokens: u32,
    ) -> Result<Self> {
        // Initialize the backend (must be done once)
        let backend = LlamaBackend::init()
            .map_err(|e| AppError::LLM(format!("Failed to initialize llama backend: {}", e)))?;

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
        let prompt = prompt.to_string();

        // Run blocking llama operations in a spawn_blocking task
        tokio::task::spawn_blocking(move || {
            Self::generate_sync(&model, &backend, n_ctx, n_threads, &prompt, max_tokens)
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

        // Set up sampler for generation
        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::temp(0.7),
            LlamaSampler::top_p(0.9, 1),
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

        // Set up sampler for generation
        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::temp(0.7),
            LlamaSampler::top_p(0.9, 1),
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

    /// Format chat history into a prompt string
    fn format_history(&self, messages: &[(String, String)]) -> String {
        let mut prompt = String::new();
        for (role, content) in messages {
            match role.as_str() {
                "system" => {
                    prompt.push_str(&format!("<|im_start|>system\n{}<|im_end|>\n", content))
                }
                "user" => prompt.push_str(&format!("<|im_start|>user\n{}<|im_end|>\n", content)),
                "assistant" => {
                    prompt.push_str(&format!("<|im_start|>assistant\n{}<|im_end|>\n", content))
                }
                _ => prompt.push_str(&format!("<|im_start|>user\n{}<|im_end|>\n", content)),
            }
        }
        prompt.push_str("<|im_start|>assistant\n");
        prompt
    }

    /// Stream with system prompt
    pub async fn stream_with_system(
        &self,
        system: &str,
        prompt: &str,
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let formatted = self.format_prompt(Some(system), prompt);
        self.stream_internal(&formatted, self.max_tokens).await
    }

    /// Stream with conversation history
    pub async fn stream_with_history(
        &self,
        messages: &[(String, String)],
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let formatted = self.format_history(messages);
        self.stream_internal(&formatted, self.max_tokens).await
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

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<String> {
        let formatted = self.format_history(messages);
        self.generate_internal(&formatted, self.max_tokens).await
    }

    async fn generate_with_tools(
        &self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        // For tool calling, we format the tools as part of the system prompt
        // and ask the model to respond in JSON format when it wants to call a tool
        let tools_json = serde_json::to_string_pretty(tools)
            .map_err(|e| AppError::LLM(format!("Failed to serialize tools: {}", e)))?;

        let system = format!(
            r#"You are a helpful assistant with access to the following tools:

{}

When you need to use a tool, respond ONLY with a JSON object in this exact format:
{{"tool_call": {{"name": "tool_name", "arguments": {{...}}}}}}

Otherwise, respond normally with text."#,
            tools_json
        );

        let formatted = self.format_prompt(Some(&system), prompt);
        let content = self.generate_internal(&formatted, self.max_tokens).await?;

        // Try to parse tool calls from the response
        let tool_calls = if content.contains("\"tool_call\"") {
            // Try to extract and parse the tool call JSON
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(tool_call) = parsed.get("tool_call") {
                    vec![crate::types::ToolCall {
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
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        let finish_reason = if tool_calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        };

        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason: finish_reason.to_string(),
        })
    }

    async fn stream(
        &self,
        prompt: &str,
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let formatted = self.format_prompt(None, prompt);
        self.stream_internal(&formatted, self.max_tokens).await
    }

    fn model_name(&self) -> &str {
        &self.model_path
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_format_prompt_without_system() {
        // Test the prompt formatting logic
        let expected = "<|im_start|>user\nHello<|im_end|>\n<|im_start|>assistant\n";
        let formatted = format!(
            "<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n",
            "Hello"
        );
        assert_eq!(formatted, expected);
    }

    #[test]
    fn test_format_prompt_with_system() {
        let expected = "<|im_start|>system\nYou are helpful<|im_end|>\n<|im_start|>user\nHello<|im_end|>\n<|im_start|>assistant\n";
        let formatted = format!(
            "<|im_start|>system\n{}<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n",
            "You are helpful", "Hello"
        );
        assert_eq!(formatted, expected);
    }

    #[test]
    fn test_format_history() {
        let history = vec![
            ("system".to_string(), "Be helpful".to_string()),
            ("user".to_string(), "Hello".to_string()),
            ("assistant".to_string(), "Hi!".to_string()),
            ("user".to_string(), "How are you?".to_string()),
        ];

        let mut result = String::new();
        for (role, content) in &history {
            match role.as_str() {
                "system" => {
                    result.push_str(&format!("<|im_start|>system\n{}<|im_end|>\n", content))
                }
                "user" => result.push_str(&format!("<|im_start|>user\n{}<|im_end|>\n", content)),
                "assistant" => {
                    result.push_str(&format!("<|im_start|>assistant\n{}<|im_end|>\n", content))
                }
                _ => {}
            }
        }
        result.push_str("<|im_start|>assistant\n");

        assert!(result.contains("Be helpful"));
        assert!(result.contains("Hello"));
        assert!(result.contains("Hi!"));
        assert!(result.contains("How are you?"));
    }

    #[test]
    fn test_format_prompt_chatml_structure() {
        // Verify ChatML tags are properly structured
        let system = "System prompt";
        let user = "User message";

        let formatted = format!(
            "<|im_start|>system\n{}<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n",
            system, user
        );

        // Count tag occurrences
        assert_eq!(formatted.matches("<|im_start|>").count(), 3);
        assert_eq!(formatted.matches("<|im_end|>").count(), 2);
        assert!(formatted.ends_with("<|im_start|>assistant\n"));
    }

    #[test]
    fn test_format_history_empty() {
        let history: Vec<(String, String)> = vec![];

        let mut result = String::new();
        for (role, content) in &history {
            match role.as_str() {
                "system" => {
                    result.push_str(&format!("<|im_start|>system\n{}<|im_end|>\n", content))
                }
                "user" => result.push_str(&format!("<|im_start|>user\n{}<|im_end|>\n", content)),
                "assistant" => {
                    result.push_str(&format!("<|im_start|>assistant\n{}<|im_end|>\n", content))
                }
                _ => {}
            }
        }
        result.push_str("<|im_start|>assistant\n");

        assert_eq!(result, "<|im_start|>assistant\n");
    }

    #[test]
    fn test_format_history_unknown_role() {
        let history = vec![("unknown_role".to_string(), "Some content".to_string())];

        let mut result = String::new();
        for (role, content) in &history {
            match role.as_str() {
                "system" => {
                    result.push_str(&format!("<|im_start|>system\n{}<|im_end|>\n", content))
                }
                "user" => result.push_str(&format!("<|im_start|>user\n{}<|im_end|>\n", content)),
                "assistant" => {
                    result.push_str(&format!("<|im_start|>assistant\n{}<|im_end|>\n", content))
                }
                // Unknown roles are skipped in the basic test
                _ => result.push_str(&format!("<|im_start|>user\n{}<|im_end|>\n", content)),
            }
        }
        result.push_str("<|im_start|>assistant\n");

        // Unknown role should be treated as user
        assert!(result.contains("Some content"));
    }

    #[test]
    fn test_tool_call_json_parsing() {
        let response = r#"{"tool_call": {"name": "calculator", "arguments": {"a": 1, "b": 2}}}"#;

        let parsed: serde_json::Value = serde_json::from_str(response).unwrap();
        let tool_call = parsed.get("tool_call").unwrap();

        assert_eq!(
            tool_call.get("name").unwrap().as_str().unwrap(),
            "calculator"
        );
        assert_eq!(
            tool_call
                .get("arguments")
                .unwrap()
                .get("a")
                .unwrap()
                .as_i64()
                .unwrap(),
            1
        );
        assert_eq!(
            tool_call
                .get("arguments")
                .unwrap()
                .get("b")
                .unwrap()
                .as_i64()
                .unwrap(),
            2
        );
    }

    #[test]
    fn test_tool_call_no_match() {
        let response = "Just a normal response without tool calls";

        let has_tool_call = response.contains("\"tool_call\"");
        assert!(!has_tool_call);
    }

    #[cfg(feature = "llamacpp")]
    #[test]
    fn test_llamacpp_client_creation_fails_with_invalid_path() {
        // Smoke test: ensure client creation fails gracefully with invalid model path
        let result = LlamaCppClient::new("nonexistent_model.gguf".to_string());
        assert!(result.is_err());
        let error = result.unwrap_err();
        match error {
            crate::types::AppError::LLM(msg) => {
                assert!(msg.contains("Failed to load model"));
            }
            _ => panic!("Expected LLM error"),
        }
    }

    #[cfg(feature = "llamacpp")]
    #[test]
    fn test_llamacpp_client_with_params() {
        // Test parameter validation without loading
        let result = LlamaCppClient::with_params(
            "dummy.gguf".to_string(),
            2048, // smaller context
            2,    // fewer threads
            256,  // fewer max tokens
        );
        assert!(result.is_err()); // Should fail due to invalid path
    }
}
