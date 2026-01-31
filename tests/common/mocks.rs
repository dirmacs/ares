//! Mock implementations for testing.
//!
//! This module provides mock LLM clients and factories that can be used
//! across different test files without duplication.

use ares::llm::client::{LLMClientFactoryTrait, Provider};
use ares::llm::{LLMClient, LLMResponse};
use ares::types::{AppError, Result, ToolCall, ToolDefinition};
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use std::sync::Arc;

/// Mock LLM client for testing with configurable responses.
///
/// This client can be configured to return specific responses, tool calls,
/// or to simulate failures. It's useful for unit testing code that depends
/// on LLM responses without making actual API calls.
///
/// # Examples
///
/// ```
/// use tests::common::mocks::MockLLMClient;
///
/// // Create a client that returns a simple response
/// let client = MockLLMClient::new("Hello, world!");
///
/// // Create a client that simulates tool calls
/// let client = MockLLMClient::with_tool_calls("response", vec![...]);
///
/// // Create a client that always fails
/// let client = MockLLMClient::failing();
/// ```
#[derive(Clone)]
pub struct MockLLMClient {
    response: String,
    tool_calls: Vec<ToolCall>,
    should_fail: bool,
}

impl MockLLMClient {
    /// Create a new mock client that returns the given response.
    pub fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
            tool_calls: vec![],
            should_fail: false,
        }
    }

    /// Create a mock client that returns both a response and tool calls.
    pub fn with_tool_calls(response: &str, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            response: response.to_string(),
            tool_calls,
            should_fail: false,
        }
    }

    /// Create a mock client that always returns an error.
    pub fn failing() -> Self {
        Self {
            response: String::new(),
            tool_calls: vec![],
            should_fail: true,
        }
    }
}

#[async_trait]
impl LLMClient for MockLLMClient {
    async fn generate(&self, _prompt: &str) -> Result<String> {
        if self.should_fail {
            return Err(AppError::LLM("Mock LLM failure".to_string()));
        }
        Ok(self.response.clone())
    }

    async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
        if self.should_fail {
            return Err(AppError::LLM("Mock LLM failure".to_string()));
        }
        Ok(self.response.clone())
    }

    async fn generate_with_history(&self, _messages: &[(String, String)]) -> Result<String> {
        if self.should_fail {
            return Err(AppError::LLM("Mock LLM failure".to_string()));
        }
        Ok(self.response.clone())
    }

    async fn generate_with_tools(
        &self,
        _prompt: &str,
        _tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        if self.should_fail {
            return Err(AppError::LLM("Mock LLM failure".to_string()));
        }

        let finish_reason = if self.tool_calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        };

        Ok(LLMResponse {
            content: self.response.clone(),
            tool_calls: self.tool_calls.clone(),
            finish_reason: finish_reason.to_string(),
            usage: None,
        })
    }

    async fn stream(
        &self,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        if self.should_fail {
            return Err(AppError::LLM("Mock LLM failure".to_string()));
        }

        let response = self.response.clone();
        // Split response into chunks for streaming simulation
        let chunks: Vec<String> = response
            .chars()
            .collect::<Vec<_>>()
            .chunks(5)
            .map(|c| c.iter().collect())
            .collect();

        let stream = stream::iter(chunks.into_iter().map(Ok));
        Ok(Box::new(stream.boxed()))
    }

    async fn stream_with_system(
        &self,
        _system: &str,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        if self.should_fail {
            return Err(AppError::LLM("Mock LLM failure".to_string()));
        }

        let response = self.response.clone();
        let chunks: Vec<String> = response
            .chars()
            .collect::<Vec<_>>()
            .chunks(5)
            .map(|c| c.iter().collect())
            .collect();

        let stream = stream::iter(chunks.into_iter().map(Ok));
        Ok(Box::new(stream.boxed()))
    }

    async fn stream_with_history(
        &self,
        _messages: &[(String, String)],
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        if self.should_fail {
            return Err(AppError::LLM("Mock LLM failure".to_string()));
        }

        let response = self.response.clone();
        let chunks: Vec<String> = response
            .chars()
            .collect::<Vec<_>>()
            .chunks(5)
            .map(|c| c.iter().collect())
            .collect();

        let stream = stream::iter(chunks.into_iter().map(Ok));
        Ok(Box::new(stream.boxed()))
    }

    fn model_name(&self) -> &str {
        "mock-model"
    }
}

/// Mock LLM factory for tests requiring complete isolation from external services.
///
/// This factory always returns instances of `MockLLMClient`, allowing tests
/// to run without any network dependencies.
pub struct MockLLMFactory {
    provider: Provider,
    client: Arc<MockLLMClient>,
}

impl MockLLMFactory {
    /// Create a new mock factory that returns the given mock client.
    pub fn new(client: MockLLMClient) -> Self {
        Self {
            provider: Provider::Ollama {
                base_url: "http://localhost:11434".to_string(),
                model: "mock".to_string(),
                params: Default::default(),
            },
            client: Arc::new(client),
        }
    }
}

#[async_trait]
impl LLMClientFactoryTrait for MockLLMFactory {
    fn default_provider(&self) -> &Provider {
        &self.provider
    }

    async fn create_default(&self) -> Result<Box<dyn LLMClient>> {
        Ok(Box::new((*self.client).clone()))
    }

    async fn create_with_provider(&self, _provider: Provider) -> Result<Box<dyn LLMClient>> {
        Ok(Box::new((*self.client).clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_client_generate() {
        let client = MockLLMClient::new("test response");
        let result = client.generate("prompt").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "test response");
    }

    #[tokio::test]
    async fn test_mock_client_failing() {
        let client = MockLLMClient::failing();
        let result = client.generate("prompt").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mock_factory() {
        let client = MockLLMClient::new("factory response");
        let factory = MockLLMFactory::new(client);

        let llm = factory.create_default().await.unwrap();
        let result = llm.generate("test").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "factory response");
    }
}
