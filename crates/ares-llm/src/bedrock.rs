//! AWS Bedrock Converse LLM client implementation.
//!
//! This module calls the Bedrock Runtime Converse API using the provider-agnostic
//! Bedrock message and tool schema.

use crate::client::{LLMClient, LLMResponse, ModelParams, TokenUsage};
use crate::coordinator::{ConversationMessage, MessageRole};
use ares_types::types::{AppError, Result, ToolCall, ToolDefinition};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Prefix accepted in ARES model config strings for direct Bedrock routing.
pub const MODEL_PREFIX: &str = "bedrock/";

fn empty_json_object() -> Value {
    Value::Object(serde_json::Map::new())
}

/// AWS Bedrock client for Converse inference.
pub struct BedrockClient {
    http: reqwest::Client,
    api_key: String,
    region: String,
    model: String,
    params: ModelParams,
}

#[derive(Debug, Serialize)]
struct ConverseRequest {
    messages: Vec<ConverseMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<ConverseTextBlock>>,
    #[serde(rename = "inferenceConfig")]
    inference_config: ConverseInferenceConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "toolConfig")]
    tool_config: Option<ConverseToolConfig>,
}

#[derive(Debug, Serialize)]
struct ConverseInferenceConfig {
    #[serde(rename = "maxTokens")]
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(rename = "topP", skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
}

#[derive(Debug, Serialize)]
struct ConverseMessage {
    role: &'static str,
    content: Vec<ConverseContentBlock>,
}

#[derive(Debug, Serialize)]
struct ConverseTextBlock {
    text: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum ConverseContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        #[serde(rename = "toolUse")]
        tool_use: ConverseToolUse,
    },
    ToolResult {
        #[serde(rename = "toolResult")]
        tool_result: ConverseToolResult,
    },
    Other(Value),
}

#[derive(Debug, Serialize)]
struct ConverseToolConfig {
    tools: Vec<ConverseTool>,
}

#[derive(Debug, Serialize)]
struct ConverseTool {
    #[serde(rename = "toolSpec")]
    tool_spec: ConverseToolSpec,
}

#[derive(Debug, Serialize)]
struct ConverseToolSpec {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: ConverseToolInputSchema,
}

#[derive(Debug, Serialize)]
struct ConverseToolInputSchema {
    json: Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct ConverseToolUse {
    #[serde(rename = "toolUseId")]
    tool_use_id: String,
    name: String,
    #[serde(default = "empty_json_object")]
    input: Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct ConverseToolResult {
    #[serde(rename = "toolUseId")]
    tool_use_id: String,
    content: Vec<ConverseToolResultContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum ConverseToolResultContent {
    Json { json: Value },
    Text { text: String },
}

#[derive(Debug, Deserialize)]
struct ConverseResponse {
    #[serde(default)]
    output: Option<ConverseOutput>,
    #[serde(rename = "stopReason")]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<ConverseUsage>,
}

#[derive(Debug, Deserialize)]
struct ConverseOutput {
    #[serde(default)]
    message: Option<ConverseOutputMessage>,
}

#[derive(Debug, Deserialize)]
struct ConverseOutputMessage {
    #[serde(default)]
    content: Vec<ConverseContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ConverseUsage {
    #[serde(rename = "inputTokens", default)]
    input_tokens: u32,
    #[serde(rename = "outputTokens", default)]
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
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/converse",
            self.region, self.model
        )
    }

    fn convert_tool(tool: &ToolDefinition) -> ConverseTool {
        ConverseTool {
            tool_spec: ConverseToolSpec {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: ConverseToolInputSchema {
                    json: tool.parameters.clone(),
                },
            },
        }
    }

    fn build_request(
        &self,
        messages: Vec<ConverseMessage>,
        tools: Vec<ConverseTool>,
        system: Option<String>,
    ) -> ConverseRequest {
        let system = system
            .filter(|text| !text.is_empty())
            .map(|text| vec![ConverseTextBlock { text }]);
        let tool_config = (!tools.is_empty()).then_some(ConverseToolConfig { tools });

        ConverseRequest {
            messages,
            inference_config: ConverseInferenceConfig {
                max_tokens: self.max_tokens(),
                temperature: self.params.temperature,
                top_p: self.params.top_p,
            },
            system,
            tool_config,
        }
    }

    async fn send_request(&self, request: ConverseRequest) -> Result<ConverseResponse> {
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

    fn text_block(text: impl Into<String>) -> ConverseContentBlock {
        ConverseContentBlock::Text { text: text.into() }
    }

    fn tool_use_block(tool_call: &ToolCall) -> ConverseContentBlock {
        ConverseContentBlock::ToolUse {
            tool_use: ConverseToolUse {
                tool_use_id: tool_call.id.clone(),
                name: tool_call.name.clone(),
                input: tool_call.arguments.clone(),
            },
        }
    }

    fn tool_result_block(tool_use_id: &str, content: &str) -> ConverseContentBlock {
        ConverseContentBlock::ToolResult {
            tool_result: ConverseToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: vec![Self::tool_result_content(content)],
                status: Some("success".to_string()),
            },
        }
    }

    fn tool_result_content(content: &str) -> ConverseToolResultContent {
        match serde_json::from_str::<Value>(content) {
            Ok(value @ Value::Object(_)) => ConverseToolResultContent::Json { json: value },
            Ok(Value::String(text)) => ConverseToolResultContent::Text { text },
            Ok(value) => ConverseToolResultContent::Text {
                text: value.to_string(),
            },
            Err(_) => ConverseToolResultContent::Text {
                text: content.to_string(),
            },
        }
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
    ) -> Option<ConverseMessage> {
        match role {
            "system" => {
                Self::push_system_prompt(system_prompt, content);
                None
            }
            "assistant" => Some(ConverseMessage {
                role: "assistant",
                content: vec![Self::text_block(content)],
            }),
            _ => Some(ConverseMessage {
                role: "user",
                content: vec![Self::text_block(content)],
            }),
        }
    }

    fn message_from_conversation(
        msg: &ConversationMessage,
        system_prompt: &mut Option<String>,
    ) -> Option<ConverseMessage> {
        match msg.role {
            MessageRole::System => {
                Self::push_system_prompt(system_prompt, &msg.content);
                None
            }
            MessageRole::User => Some(ConverseMessage {
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
                Some(ConverseMessage {
                    role: "assistant",
                    content,
                })
            }
            MessageRole::Tool => {
                let tool_call_id = msg.tool_call_id.as_deref().unwrap_or_default();
                Some(ConverseMessage {
                    role: "user",
                    content: vec![Self::tool_result_block(tool_call_id, &msg.content)],
                })
            }
        }
    }

    fn extract_text_content(content: &[ConverseContentBlock]) -> String {
        let mut text = String::new();
        for block in content {
            if let ConverseContentBlock::Text { text: block_text } = block {
                text.push_str(block_text);
            }
        }
        text
    }

    fn extract_tool_calls(content: &[ConverseContentBlock]) -> Vec<ToolCall> {
        content
            .iter()
            .filter_map(|block| {
                if let ConverseContentBlock::ToolUse { tool_use } = block {
                    Some(ToolCall {
                        id: tool_use.tool_use_id.clone(),
                        name: tool_use.name.clone(),
                        arguments: tool_use.input.clone(),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    fn llm_response(response: ConverseResponse) -> LLMResponse {
        let content = response
            .output
            .and_then(|output| output.message)
            .map(|message| message.content)
            .unwrap_or_default();
        let usage = response
            .usage
            .map(|usage| TokenUsage::new(usage.input_tokens, usage.output_tokens));

        LLMResponse {
            content: Self::extract_text_content(&content),
            tool_calls: Self::extract_tool_calls(&content),
            finish_reason: response.stop_reason.unwrap_or_else(|| "stop".to_string()),
            usage,
        }
    }

    async fn generate_response(
        &self,
        messages: Vec<ConverseMessage>,
        tools: Vec<ConverseTool>,
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
                vec![ConverseMessage {
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
                vec![ConverseMessage {
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
            vec![ConverseMessage {
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
    use serde_json::json;

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
        assert_eq!(
            strip_model_prefix("bedrock/us.meta.llama3-3-70b-instruct-v1:0"),
            "us.meta.llama3-3-70b-instruct-v1:0"
        );
    }

    #[test]
    fn convert_tool_uses_converse_tool_schema() {
        let converted = BedrockClient::convert_tool(&tool_definition());
        assert_eq!(converted.tool_spec.name, "calculator");
        assert_eq!(converted.tool_spec.description, "Run a calculation");
        assert_eq!(
            converted.tool_spec.input_schema.json["required"][0],
            "expression"
        );
    }

    #[test]
    fn build_request_uses_converse_body_shape() {
        let client = BedrockClient::with_params(
            "token".to_string(),
            "us-east-1".to_string(),
            "bedrock/us.amazon.nova-lite-v1:0".to_string(),
            ModelParams {
                max_tokens: Some(4096),
                temperature: Some(0.3),
                top_p: Some(0.9),
                ..ModelParams::default()
            },
        );
        let request = client.build_request(
            vec![ConverseMessage {
                role: "user",
                content: vec![BedrockClient::text_block("hello")],
            }],
            vec![BedrockClient::convert_tool(&tool_definition())],
            Some("system prompt".to_string()),
        );
        let body = serde_json::to_value(request).expect("request serializes");

        assert_eq!(client.model_name(), "us.amazon.nova-lite-v1:0");
        assert_eq!(
            client.endpoint(),
            format!(
                "https://bedrock-runtime.{}.amazonaws.com/model/{}/converse",
                "us-east-1", "us.amazon.nova-lite-v1:0"
            )
        );
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hello");
        assert_eq!(body["system"][0]["text"], "system prompt");
        assert_eq!(body["inferenceConfig"]["maxTokens"], 4096);
        assert!(
            (body["inferenceConfig"]["temperature"]
                .as_f64()
                .expect("temperature is a number")
                - 0.3)
                .abs()
                < 0.000_001
        );
        assert!(
            (body["inferenceConfig"]["topP"]
                .as_f64()
                .expect("topP is a number")
                - 0.9)
                .abs()
                < 0.000_001
        );
        assert_eq!(
            body["toolConfig"]["tools"][0]["toolSpec"]["inputSchema"]["json"]["required"][0],
            "expression"
        );
    }

    #[test]
    fn build_request_omits_absent_system_and_tools() {
        let client = BedrockClient::new(
            "token".to_string(),
            "us-east-1".to_string(),
            "amazon.titan-text-premier-v1:0".to_string(),
        );
        let request = client.build_request(
            vec![ConverseMessage {
                role: "user",
                content: vec![BedrockClient::text_block("hello")],
            }],
            Vec::new(),
            None,
        );
        let body = serde_json::to_value(request).expect("request serializes");

        assert!(body.get("system").is_none());
        assert!(body.get("toolConfig").is_none());
        assert_eq!(body["inferenceConfig"]["maxTokens"], DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn parses_converse_response_text_tool_calls_and_usage() {
        let response: ConverseResponse = serde_json::from_value(json!({
            "output": {
                "message": {
                    "role": "assistant",
                    "content": [
                        { "text": "checking" },
                        {
                            "toolUse": {
                                "toolUseId": "toolu_1",
                                "name": "calculator",
                                "input": { "expression": "2+2" }
                            }
                        }
                    ]
                }
            },
            "stopReason": "tool_use",
            "usage": {
                "inputTokens": 12,
                "outputTokens": 7,
                "totalTokens": 19
            }
        }))
        .expect("response deserializes");

        let llm_response = BedrockClient::llm_response(response);
        assert_eq!(llm_response.content, "checking");
        assert_eq!(llm_response.finish_reason, "tool_use");
        assert_eq!(llm_response.usage, Some(TokenUsage::new(12, 7)));
        assert_eq!(llm_response.tool_calls.len(), 1);
        assert_eq!(llm_response.tool_calls[0].id, "toolu_1");
        assert_eq!(llm_response.tool_calls[0].name, "calculator");
        assert_eq!(llm_response.tool_calls[0].arguments["expression"], "2+2");
    }

    #[test]
    fn tool_result_messages_use_converse_content_blocks() {
        let msg = ConversationMessage::tool_result("toolu_1", &json!({"answer": 4}));
        let mut system_prompt = None;
        let converted = BedrockClient::message_from_conversation(&msg, &mut system_prompt)
            .expect("tool messages convert to user messages");
        let body = serde_json::to_value(converted).expect("message serializes");

        assert_eq!(body["role"], "user");
        assert_eq!(body["content"][0]["toolResult"]["toolUseId"], "toolu_1");
        assert_eq!(body["content"][0]["toolResult"]["status"], "success");
        assert_eq!(
            body["content"][0]["toolResult"]["content"][0]["json"]["answer"],
            4
        );
    }

    #[test]
    fn scalar_tool_results_fall_back_to_text_blocks() {
        let msg = ConversationMessage::tool_result("toolu_1", &json!("done"));
        let mut system_prompt = None;
        let converted = BedrockClient::message_from_conversation(&msg, &mut system_prompt)
            .expect("tool messages convert to user messages");
        let body = serde_json::to_value(converted).expect("message serializes");

        assert_eq!(
            body["content"][0]["toolResult"]["content"][0]["text"],
            "done"
        );
    }
}
