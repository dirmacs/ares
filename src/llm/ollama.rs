use crate::llm::client::{LLMClient, LLMResponse};
use crate::types::{AppError, Result, ToolCall, ToolDefinition};
use async_stream::stream;
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use ollama_rs::{
    Ollama,
    generation::chat::{ChatMessage, request::ChatMessageRequest, ChatMessageRequest as ChatRequest},
    generation::tools::Tool as OllamaTool,
};

pub struct OllamaClient {
    client: Ollama,
    model: String,
}

impl OllamaClient {
    pub async fn new(base_url: String, model: String) -> Result<Self> {
        let url_parts: Vec<&str> = base_url.split("://").collect();
        let (host, port) = if url_parts.len() == 2 {
            let host_port: Vec<&str> = url_parts[1].split(':').collect();
            let host = host_port[0].to_string();
            let port = if host_port.len() == 2 {
                host_port[1].parse().unwrap_or(11434)
            } else {
                11434
            };
            (host, port)
        } else {
            ("localhost".to_string(), 11434)
        };

        let client = Ollama::new(host, port);

        Ok(Self { client, model })
    }
}

#[async_trait]
impl LLMClient for OllamaClient {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let messages = vec![ChatMessage::user(prompt.to_string())];

        let request = ChatMessageRequest::new(self.model.clone(), messages);

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

        // ChatMessageResponse has a `message` field that is a ChatMessage (not Option)
        // ChatMessage has a `content` field that is a String
        Ok(response.message.content)
    }

    async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        let messages = vec![
            ChatMessage::system(system.to_string()),
            ChatMessage::user(prompt.to_string()),
        ];

        let request = ChatMessageRequest::new(self.model.clone(), messages);

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

        Ok(response.message.content)
    }

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<String> {
        let chat_messages: Vec<ChatMessage> = messages
            .iter()
            .map(|(role, content)| match role.as_str() {
                "system" => ChatMessage::system(content.clone()),
                "user" => ChatMessage::user(content.clone()),
                "assistant" => ChatMessage::assistant(content.clone()),
                _ => ChatMessage::user(content.clone()),
            })
            .collect();

        let request = ChatMessageRequest::new(self.model.clone(), chat_messages);

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

        Ok(response.message.content)
    }

    async fn generate_with_tools(
        &self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        // Ollama supports function calling for compatible models (llama3.1+, mistral-nemo, etc.)
        // If no tools provided, fall back to regular generation
        if tools.is_empty() {
            let content = self.generate(prompt).await?;
            return Ok(LLMResponse {
                content,
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
            });
        }

        // Convert our ToolDefinition to Ollama's Tool format
        let ollama_tools: Vec<OllamaTool> = tools
            .iter()
            .map(|tool| {
                // Ollama expects tools in OpenAI function calling format
                OllamaTool {
                    function: ollama_rs::generation::tools::Function {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: tool.parameters.clone(),
                    },
                    r#type: "function".to_string(),
                }
            })
            .collect();

        let messages = vec![ChatMessage::user(prompt.to_string())];
        
        let mut request = ChatMessageRequest::new(self.model.clone(), messages);
        request = request.tools(ollama_tools);

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama tool calling error: {}", e)))?;

        // Extract tool calls if present
        let tool_calls: Vec<ToolCall> = if let Some(calls) = response.message.tool_calls {
            calls
                .into_iter()
                .enumerate()
                .map(|(idx, call)| ToolCall {
                    id: format!("call_{}", idx),
                    name: call.function.name,
                    arguments: call.function.arguments,
                })
                .collect()
        } else {
            vec![]
        };

        // Determine finish reason
        let finish_reason = if !tool_calls.is_empty() {
            "tool_calls".to_string()
        } else if let Some(reason) = response.done_reason {
            reason
        } else {
            "stop".to_string()
        };

        Ok(LLMResponse {
            content: response.message.content,
            tool_calls,
            finish_reason,
        })
    }

    async fn stream(
        &self,
        prompt: &str,
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let messages = vec![ChatMessage::user(prompt.to_string())];
        let request = ChatMessageRequest::new(self.model.clone(), messages);

        let mut stream_response = self
            .client
            .send_chat_messages_stream(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama stream error: {}", e)))?;

        // Create an async stream that yields content chunks
        let output_stream = stream! {
            while let Some(chunk_result) = stream_response.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        // Each chunk contains a message with content
                        let content = chunk.message.content;
                        if !content.is_empty() {
                            yield Ok(content);
                        }
                    }
                    Err(_) => {
                        yield Err(AppError::LLM("Stream chunk error".to_string()));
                        break;
                    }
                }
            }
        };

        Ok(Box::new(Box::pin(output_stream)))
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_url_parsing_full() {
        // This tests the URL parsing logic
        let base_url = "http://localhost:11434";
        let url_parts: Vec<&str> = base_url.split("://").collect();
        assert_eq!(url_parts.len(), 2);
        assert_eq!(url_parts[0], "http");
        assert_eq!(url_parts[1], "localhost:11434");

        let host_port: Vec<&str> = url_parts[1].split(':').collect();
        assert_eq!(host_port[0], "localhost");
        assert_eq!(host_port[1], "11434");
    }

    #[test]
    fn test_url_parsing_no_port() {
        let base_url = "http://localhost";
        let url_parts: Vec<&str> = base_url.split("://").collect();
        let host_port: Vec<&str> = url_parts[1].split(':').collect();

        let host = host_port[0].to_string();
        let port = if host_port.len() == 2 {
            host_port[1].parse().unwrap_or(11434)
        } else {
            11434
        };

        assert_eq!(host, "localhost");
        assert_eq!(port, 11434);
    }

    #[test]
    fn test_url_parsing_custom_port() {
        let base_url = "http://192.168.1.100:8080";
        let url_parts: Vec<&str> = base_url.split("://").collect();
        let host_port: Vec<&str> = url_parts[1].split(':').collect();

        let host = host_port[0].to_string();
        let port: u16 = host_port[1].parse().unwrap_or(11434);

        assert_eq!(host, "192.168.1.100");
        assert_eq!(port, 8080);
    }

    #[test]
    fn test_tool_definition_conversion() {
        let tool_def = ToolDefinition {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "arg1": {
                        "type": "string",
                        "description": "First argument"
                    }
                },
                "required": ["arg1"]
            }),
        };

        let ollama_tool = OllamaTool {
            function: ollama_rs::generation::tools::Function {
                name: tool_def.name.clone(),
                description: tool_def.description.clone(),
                parameters: tool_def.parameters.clone(),
            },
            r#type: "function".to_string(),
        };

        assert_eq!(ollama_tool.function.name, "test_tool");
        assert_eq!(ollama_tool.function.description, "A test tool");
        assert_eq!(ollama_tool.r#type, "function");
    }
}
