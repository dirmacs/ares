use ares::llm::*;
use ares::types::{Result, ToolDefinition};
use async_trait::async_trait;

/// Mock LLM client for testing
struct MockLLMClient {
    response: String,
}

impl MockLLMClient {
    fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
        }
    }
}

#[async_trait]
impl LLMClient for MockLLMClient {
    async fn generate(&self, _prompt: &str) -> Result<String> {
        Ok(self.response.clone())
    }

    async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
        Ok(self.response.clone())
    }

    async fn generate_with_history(&self, _messages: &[(String, String)]) -> Result<String> {
        Ok(self.response.clone())
    }

    async fn generate_with_tools(
        &self,
        _prompt: &str,
        _tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        Ok(LLMResponse {
            content: self.response.clone(),
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
        })
    }

    async fn stream(
        &self,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        use futures::stream::{self, StreamExt};
        let response = self.response.clone();
        Ok(Box::new(stream::once(async move { Ok(response) }).boxed()))
    }

    fn model_name(&self) -> &str {
        "mock-model"
    }
}

#[tokio::test]
async fn test_mock_llm_client_generate() {
    let client = MockLLMClient::new("Hello, world!");
    let result = client.generate("test prompt").await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "Hello, world!");
}

#[tokio::test]
async fn test_mock_llm_client_generate_with_system() {
    let client = MockLLMClient::new("System response");
    let result = client
        .generate_with_system("You are a helpful assistant", "Hello")
        .await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "System response");
}

#[tokio::test]
async fn test_mock_llm_client_generate_with_history() {
    let client = MockLLMClient::new("History response");
    let messages = vec![
        ("user".to_string(), "Hello".to_string()),
        ("assistant".to_string(), "Hi there!".to_string()),
    ];
    let result = client.generate_with_history(&messages).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "History response");
}

#[tokio::test]
async fn test_mock_llm_client_generate_with_tools() {
    let client = MockLLMClient::new("Tool response");
    let tools: Vec<ToolDefinition> = vec![];
    let result = client.generate_with_tools("Use tools", &tools).await;
    assert!(result.is_ok());
    let response = result.unwrap();
    assert_eq!(response.content, "Tool response");
    assert_eq!(response.finish_reason, "stop");
    assert!(response.tool_calls.is_empty());
}

#[tokio::test]
async fn test_mock_llm_client_model_name() {
    let client = MockLLMClient::new("test");
    assert_eq!(client.model_name(), "mock-model");
}
