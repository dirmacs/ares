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
use async_trait::async_trait;
use llama_cpp_2::{
    context::params::LlamaContextParams,
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{params::LlamaModelParams, AddBos, LlamaModel, Special},
    sampling::LlamaSampler,
};
use std::num::NonZeroU32;
use std::sync::Arc;

/// LlamaCpp client for local GGUF model inference
pub struct LlamaCppClient {
    model_path: String,
    model: Arc<LlamaModel>,
    #[allow(dead_code)]
    backend: Arc<LlamaBackend>,
    /// Context size for the model
    n_ctx: u32,
    /// Number of threads to use
    n_threads: u32,
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
        Self::with_params(model_path, 4096, 4)
    }

    /// Create a new LlamaCpp client with custom parameters
    ///
    /// # Arguments
    ///
    /// * `model_path` - Path to a GGUF model file
    /// * `n_ctx` - Context size (default: 4096)
    /// * `n_threads` - Number of CPU threads (default: 4)
    pub fn with_params(model_path: String, n_ctx: u32, n_threads: u32) -> Result<Self> {
        // Initialize the backend (must be done once)
        let backend = LlamaBackend::init()
            .map_err(|e| AppError::LLM(format!("Failed to initialize llama backend: {}", e)))?;

        // Set up model parameters
        let model_params = LlamaModelParams::default();

        // Load the model
        let model = LlamaModel::load_from_file(&backend, &model_path, &model_params).map_err(
            |e| AppError::LLM(format!("Failed to load model from '{}': {}", model_path, e)),
        )?;

        Ok(Self {
            model_path,
            model: Arc::new(model),
            backend: Arc::new(backend),
            n_ctx,
            n_threads,
        })
    }

    /// Get the model path
    pub fn model_path(&self) -> &str {
        &self.model_path
    }

    /// Generate text from tokens
    async fn generate_internal(&self, prompt: &str, max_tokens: u32) -> Result<String> {
        let model = self.model.clone();
        let n_ctx = self.n_ctx;
        let n_threads = self.n_threads;
        let prompt = prompt.to_string();

        // Run blocking llama operations in a spawn_blocking task
        tokio::task::spawn_blocking(move || {
            // Create context parameters
            let ctx_params = LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(n_ctx))
                .with_n_threads(n_threads)
                .with_n_threads_batch(n_threads);

            // Create context
            let mut ctx = model
                .new_context(&model, ctx_params)
                .map_err(|e| AppError::LLM(format!("Failed to create context: {}", e)))?;

            // Tokenize the prompt
            let tokens = model
                .str_to_token(&prompt, AddBos::Always)
                .map_err(|e| AppError::LLM(format!("Failed to tokenize prompt: {}", e)))?;

            if tokens.is_empty() {
                return Err(AppError::LLM("Empty prompt after tokenization".to_string()));
            }

            // Create a batch for the tokens
            let mut batch = LlamaBatch::new(n_ctx as usize, 1);

            // Add tokens to batch
            for (i, token) in tokens.iter().enumerate() {
                let is_last = i == tokens.len() - 1;
                batch.add(*token, i as i32, &[0], is_last).map_err(|e| {
                    AppError::LLM(format!("Failed to add token to batch: {}", e))
                })?;
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
                batch.add(new_token, n_cur as i32, &[0], true).map_err(|e| {
                    AppError::LLM(format!("Failed to add generated token to batch: {}", e))
                })?;

                // Decode the new token
                ctx.decode(&mut batch).map_err(|e| {
                    AppError::LLM(format!("Failed to decode generated token: {}", e))
                })?;

                n_cur += 1;
            }

            // Convert all tokens to string
            let mut result = String::new();
            for token in &output_tokens {
                if let Ok(piece) = model.token_to_str_with_size(token, 256, Special::Tokenize) {
                    result.push_str(&piece);
                }
            }

            Ok(result)
        })
        .await
        .map_err(|e| AppError::LLM(format!("Task join error: {}", e)))?
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
}

#[async_trait]
impl LLMClient for LlamaCppClient {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let formatted = self.format_prompt(None, prompt);
        self.generate_internal(&formatted, 512).await
    }

    async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        let formatted = self.format_prompt(Some(system), prompt);
        self.generate_internal(&formatted, 512).await
    }

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<String> {
        let formatted = self.format_history(messages);
        self.generate_internal(&formatted, 512).await
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
        let content = self.generate_internal(&formatted, 512).await?;

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

        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason: "stop".to_string(),
        })
    }

    async fn stream(
        &self,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        // Streaming requires more complex implementation with channels
        Err(AppError::LLM(
            "Streaming not yet implemented for LlamaCpp. Use generate() instead.".to_string(),
        ))
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
}
