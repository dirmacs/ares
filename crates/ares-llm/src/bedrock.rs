//! AWS Bedrock Claude LLM client implementation.
//!
//! This module calls the Bedrock Runtime `invoke` endpoint with Anthropic
//! Messages API request/response bodies.

use crate::client::{LLMClient, LLMResponse, ModelParams, TokenUsage};
use crate::coordinator::{ConversationMessage, MessageRole};
use ares_types::types::{AppError, Result, ToolCall, ToolDefinition};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";
const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Prefix accepted in ARES model config strings for direct Bedrock routing.
pub const MODEL_PREFIX: &str = "bedrock/";

/// AWS Bedrock client for Claude inference.
pub struct BedrockClient {
    http: reqwest::Client,
    api_key: String,
    region: String,
    model: String,
    params: ModelParams,
}

#[derive(Debug, Serialize)]
struct BedrockRequest {
    anthropic_version: &'static str,
    max_tokens: u32,
    messages: Vec<BedrockMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<BedrockTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
}

#[derive(Debug, Serialize)]
struct BedrockMessage {
    role: &'static str,
    content: Vec<Value>,
}

#[derive(Debug, Serialize)]
struct BedrockTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Debug, Deserialize)]
struct BedrockResponse {
    #[serde(default)]
    content: Vec<Value>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<BedrockUsage>,
}

#[derive(Debug, Deserialize)]
struct BedrockUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

impl BedrockClient {
    /// Create a new Bedrock client.
    pub fn new(api_key: String, region: String, model: String) -> Self {
        Self::with_params(api_key, region, model, ModelParams::default())
    }

    /// Create a new Bedrock client with model parameters.
    pub fn with_params(
        api_key: String,
        region: String,
        model: String,
        params: ModelParams,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            region,
            model: strip_model_prefix(&model).to_string(),
            params,
        }
    }

    fn max_tokens(&self) -> u32 {
        self.params.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)
    }

    fn endpoint(&self) -> String {
        format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke",
            self.region, self.model
        )
    }

    fn convert_tool(tool: &ToolDefinition) -> BedrockTool {
        BedrockTool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.parameters.clone(),
        }
    }

    fn build_request(
        &self,
        messages: Vec<BedrockMessage>,
        tools: Vec<BedrockTool>,
        system: Option<String>,
    ) -> BedrockRequest {
        BedrockRequest {
            anthropic_version: ANTHROPIC_VERSION,
            max_tokens: self.max_tokens(),
            messages,
            system,
            tool_choice: (!tools.is_empty()).then(|| json!({ "type": "auto" })),
            tools,
            temperature: self.params.temperature,
            top_p: self.params.top_p,
        }
    }

    async fn send_request(&self, request: BedrockRequest) -> Result<BedrockResponse> {
        let response = self
            .http
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| AppError::LLM(format!("Bedrock API request failed: {e}")))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| AppError::LLM(format!("Bedrock API response read failed: {e}")))?;

        if !status.is_success() {
            return Err(AppError::LLM(format!(
                "Bedrock API error (HTTP {}): {}",
                status.as_u16(),
                body
            )));
        }

        serde_json::from_str(&body)
            .map_err(|e| AppError::LLM(format!("Bedrock API response parse failed: {e}")))
    }

    fn text_block(text: impl Into<String>) -> Value {
        json!({ "type": "text", "text": text.into() })
    }

    fn tool_use_block(tool_call: &ToolCall) -> Value {
        json!({
            "type": "tool_use",
            "id": tool_call.id.clone(),
            "name": tool_call.name.clone(),
            "input": tool_call.arguments.clone(),
        })
    }

    fn tool_result_block(tool_use_id: &str, content: &str) -> Value {
        json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
        })
    }

    fn push_system_prompt(system_prompt: &mut Option<String>, content: &str) {
        match system_prompt {
            Some(existing) if !existing.is_empty() => {
                existing.push_str("\n\n");
                existing.push_str(content);
            }
            Some(existing) => existing.push_str(content),
            None => *system_prompt = Some(content.to_string()),
        }
    }

    fn message_from_role_content(
        role: &str,
        content: &str,
        system_prompt: &mut Option<String>,
    ) -> Option<BedrockMessage> {
        match role {
            "system" => {
                Self::push_system_prompt(system_prompt, content);
                None
            }
            "assistant" => Some(BedrockMessage {
                role: "assistant",
                content: vec![Self::text_block(content)],
            }),
            _ => Some(BedrockMessage {
                role: "user",
                content: vec![Self::text_block(content)],
            }),
        }
    }

    fn message_from_conversation(
        msg: &ConversationMessage,
        system_prompt: &mut Option<String>,
    ) -> Option<BedrockMessage> {
        match msg.role {
            MessageRole::System => {
                Self::push_system_prompt(system_prompt, &msg.content);
                None
            }
            MessageRole::User => Some(BedrockMessage {
                role: "user",
                content: vec![Self::text_block(&msg.content)],
            }),
            MessageRole::Assistant => {
                let mut content = Vec::new();
                if !msg.content.is_empty() {
                    content.push(Self::text_block(&msg.content));
                }
                content.extend(msg.tool_calls.iter().map(Self::tool_use_block));
                if content.is_empty() {
                    content.push(Self::text_block(""));
                }
                Some(BedrockMessage {
                    role: "assistant",
                    content,
                })
            }
            MessageRole::Tool => {
                let tool_call_id = msg.tool_call_id.as_deref().unwrap_or_default();
                Some(BedrockMessage {
                    role: "user",
                    content: vec![Self::tool_result_block(tool_call_id, &msg.content)],
                })
            }
        }
    }

    fn extract_text_content(content: &[Value]) -> String {
        content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("")
    }

    fn extract_tool_calls(content: &[Value]) -> Vec<ToolCall> {
        content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
            .filter_map(|block| {
                let id = block.get("id")?.as_str()?.to_string();
                let name = block.get("name")?.as_str()?.to_string();
                let arguments = block.get("input").cloned().unwrap_or_else(|| json!({}));
                Some(ToolCall {
                    id,
                    name,
                    arguments,
                })
            })
            .collect()
    }

    fn llm_response(response: BedrockResponse) -> LLMResponse {
        let usage = response
            .usage
            .map(|usage| TokenUsage::new(usage.input_tokens, usage.output_tokens));

        LLMResponse {
            content: Self::extract_text_content(&response.content),
            tool_calls: Self::extract_tool_calls(&response.content),
            finish_reason: response.stop_reason.unwrap_or_else(|| "stop".to_string()),
            usage,
        }
    }

    async fn generate_response(
        &self,
        messages: Vec<BedrockMessage>,
        tools: Vec<BedrockTool>,
        system_prompt: Option<String>,
    ) -> Result<LLMResponse> {
        let request = self.build_request(messages, tools, system_prompt);
        let response = self.send_request(request).await?;
        Ok(Self::llm_response(response))
    }

    async fn one_shot_stream(
        content: Result<String>,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let stream = futures::stream::once(async move { content });
        Ok(Box::new(Box::pin(stream)))
    }
}

/// Remove the ARES direct-routing prefix from a Bedrock model id.
pub fn strip_model_prefix(model: &str) -> &str {
    model.strip_prefix(MODEL_PREFIX).unwrap_or(model)
}

#[async_trait]
impl LLMClient for BedrockClient {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let response = self
            .generate_response(
                vec![BedrockMessage {
                    role: "user",
                    content: vec![Self::text_block(prompt)],
                }],
                Vec::new(),
                None,
            )
            .await?;
        Ok(response.content)
    }

    async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        let response = self
            .generate_response(
                vec![BedrockMessage {
                    role: "user",
                    content: vec![Self::text_block(prompt)],
                }],
                Vec::new(),
                Some(system.to_string()),
            )
            .await?;
        Ok(response.content)
    }

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<LLMResponse> {
        let mut system_prompt = None;
        let bedrock_messages = messages
            .iter()
            .filter_map(|(role, content)| {
                Self::message_from_role_content(role, content, &mut system_prompt)
            })
            .collect();

        self.generate_response(bedrock_messages, Vec::new(), system_prompt)
            .await
    }

    async fn generate_with_tools(
        &self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        let bedrock_tools = tools.iter().map(Self::convert_tool).collect();
        self.generate_response(
            vec![BedrockMessage {
                role: "user",
                content: vec![Self::text_block(prompt)],
            }],
            bedrock_tools,
            None,
        )
        .await
    }

    async fn generate_with_tools_and_history(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        let bedrock_tools = tools.iter().map(Self::convert_tool).collect();
        let mut system_prompt = None;
        let bedrock_messages = messages
            .iter()
            .filter_map(|msg| Self::message_from_conversation(msg, &mut system_prompt))
            .collect();

        self.generate_response(bedrock_messages, bedrock_tools, system_prompt)
            .await
    }

    async fn stream(
        &self,
        prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let content = self.generate(prompt).await;
        Self::one_shot_stream(content).await
    }

    async fn stream_with_system(
        &self,
        system: &str,
        prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let content = self.generate_with_system(system, prompt).await;
        Self::one_shot_stream(content).await
    }

    async fn stream_with_history(
        &self,
        messages: &[(String, String)],
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let content = self
            .generate_with_history(messages)
            .await
            .map(|r| r.content);
        Self::one_shot_stream(content).await
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_definition() -> ToolDefinition {
        ToolDefinition {
            name: "calculator".to_string(),
            description: "Run a calculation".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "expression": { "type": "string" }
                },
                "required": ["expression"]
            }),
        }
    }

    #[test]
    fn strip_model_prefix_accepts_prefixed_and_raw_model_ids() {
        assert_eq!(
            strip_model_prefix("bedrock/us.anthropic.claude-haiku-4-5-20251001-v1:0"),
            "us.anthropic.claude-haiku-4-5-20251001-v1:0"
        );
        assert_eq!(
            strip_model_prefix("us.anthropic.claude-haiku-4-5-20251001-v1:0"),
            "us.anthropic.claude-haiku-4-5-20251001-v1:0"
        );
    }

    #[test]
    fn convert_tool_uses_anthropic_tool_schema() {
        let converted = BedrockClient::convert_tool(&tool_definition());
        assert_eq!(converted.name, "calculator");
        assert_eq!(converted.description, "Run a calculation");
        assert_eq!(converted.input_schema["required"][0], "expression");
    }

    #[test]
    fn extracts_tool_use_blocks_from_response_content() {
        let content = vec![
            json!({"type": "text", "text": "checking"}),
            json!({
                "type": "tool_use",
                "id": "toolu_1",
                "name": "calculator",
                "input": { "expression": "2+2" }
            }),
        ];

        let calls = BedrockClient::extract_tool_calls(&content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_1");
        assert_eq!(calls[0].name, "calculator");
        assert_eq!(calls[0].arguments["expression"], "2+2");
    }

    #[test]
    fn tool_result_messages_use_anthropic_content_blocks() {
        let msg = ConversationMessage::tool_result("toolu_1", &json!({"answer": 4}));
        let mut system_prompt = None;
        let converted = BedrockClient::message_from_conversation(&msg, &mut system_prompt)
            .expect("tool messages convert to user messages");

        assert_eq!(converted.role, "user");
        assert_eq!(converted.content[0]["type"], "tool_result");
        assert_eq!(converted.content[0]["tool_use_id"], "toolu_1");
        assert_eq!(converted.content[0]["content"], "{\"answer\":4}");
    }
}
