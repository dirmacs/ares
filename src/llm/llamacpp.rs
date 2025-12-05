//! LlamaCpp LLM client implementation
//!
//! This module provides integration with llama.cpp via the `llama-cpp-2` crate.
//!
//! # Status
//!
//! This provider is currently a **placeholder stub**. Full implementation requires:
//! - A compiled GGUF model file
//! - The llama-cpp-2 crate with appropriate backend features (cuda, metal, etc.)
//! - Platform-specific build configuration
//!
//! # Usage
//!
//! For local LLM inference, we recommend using **Ollama** instead, which provides:
//! - Easy model management (pull, run, list)
//! - Built-in API server
//! - Cross-platform support
//! - No manual model file management
//!
//! If you specifically need llama.cpp integration, consider:
//! 1. Using the `llama-cpp-2` crate directly in your own code
//! 2. Running llama.cpp's server and using the OpenAI-compatible API
//! 3. Contributing a full implementation to this module
//!
//! # Example (future implementation)
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

/// LlamaCpp client for local LLM inference
///
/// # Current Status
///
/// This is a placeholder implementation. All methods return an error
/// indicating that the provider is not yet fully implemented.
///
/// # Alternatives
///
/// - **Ollama**: Recommended for local LLM inference. Easy to set up and use.
/// - **OpenAI-compatible server**: Run llama.cpp server with `--api` flag
///   and use the OpenAI provider with a custom base URL.
pub struct LlamaCppClient {
    model_path: String,
}

impl LlamaCppClient {
    /// Create a new LlamaCpp client
    ///
    /// # Arguments
    ///
    /// * `model_path` - Path to a GGUF model file
    ///
    /// # Note
    ///
    /// This constructor succeeds but the client methods will return errors
    /// until a full implementation is added.
    pub fn new(model_path: String) -> Result<Self> {
        // Note: In a full implementation, this would:
        // 1. Verify the model file exists
        // 2. Load the model into memory
        // 3. Initialize the llama.cpp context
        //
        // For now, we just store the path and defer to runtime errors.
        Ok(Self { model_path })
    }

    /// Get the model path
    pub fn model_path(&self) -> &str {
        &self.model_path
    }

    /// Helper to generate the "not implemented" error with helpful context
    fn not_implemented_error(&self) -> AppError {
        AppError::LLM(format!(
            "LlamaCpp provider not implemented. Model path: '{}'. \
             Consider using Ollama instead: https://ollama.ai",
            self.model_path
        ))
    }
}

#[async_trait]
impl LLMClient for LlamaCppClient {
    async fn generate(&self, _prompt: &str) -> Result<String> {
        Err(self.not_implemented_error())
    }

    async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
        Err(self.not_implemented_error())
    }

    async fn generate_with_history(&self, _messages: &[(String, String)]) -> Result<String> {
        Err(self.not_implemented_error())
    }

    async fn generate_with_tools(
        &self,
        _prompt: &str,
        _tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        Err(self.not_implemented_error())
    }

    async fn stream(
        &self,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        Err(self.not_implemented_error())
    }

    fn model_name(&self) -> &str {
        &self.model_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = LlamaCppClient::new("/path/to/model.gguf".to_string());
        assert!(client.is_ok());

        let client = client.unwrap();
        assert_eq!(client.model_path(), "/path/to/model.gguf");
        assert_eq!(client.model_name(), "/path/to/model.gguf");
    }

    #[tokio::test]
    async fn test_generate_returns_error() {
        let client = LlamaCppClient::new("/path/to/model.gguf".to_string()).unwrap();
        let result = client.generate("test prompt").await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not implemented"));
        assert!(err.to_string().contains("Ollama"));
    }

    #[tokio::test]
    async fn test_generate_with_system_returns_error() {
        let client = LlamaCppClient::new("/path/to/model.gguf".to_string()).unwrap();
        let result = client.generate_with_system("system", "prompt").await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_generate_with_history_returns_error() {
        let client = LlamaCppClient::new("/path/to/model.gguf".to_string()).unwrap();
        let messages = vec![("user".to_string(), "hello".to_string())];
        let result = client.generate_with_history(&messages).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_generate_with_tools_returns_error() {
        let client = LlamaCppClient::new("/path/to/model.gguf".to_string()).unwrap();
        let result = client.generate_with_tools("prompt", &[]).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_stream_returns_error() {
        let client = LlamaCppClient::new("/path/to/model.gguf".to_string()).unwrap();
        let result = client.stream("prompt").await;

        assert!(result.is_err());
    }
}
