//! Pawan Agent - The core agent that handles tool-calling loops
//!
//! This module provides the main `PawanAgent` which:
//! - Manages conversation history
//! - Coordinates tool calling with the LLM
//! - Provides streaming responses
//! - Supports multiple LLM backends via ares

use crate::config::PawanConfig;
use crate::tools::{ToolDefinition, ToolRegistry};
use crate::{PawanError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

/// A message in the conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Role of the message sender
    pub role: Role,
    /// Content of the message
    pub content: String,
    /// Tool calls (if any)
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRequest>,
    /// Tool results (if this is a tool result message)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<ToolResultMessage>,
}

/// Role of a message sender
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A request to call a tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRequest {
    /// Unique ID for this tool call
    pub id: String,
    /// Name of the tool to call
    pub name: String,
    /// Arguments for the tool
    pub arguments: Value,
}

/// Result from a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultMessage {
    /// ID of the tool call this result is for
    pub tool_call_id: String,
    /// The result content
    pub content: Value,
    /// Whether the tool executed successfully
    pub success: bool,
}

/// Record of a tool call execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Unique ID for this tool call
    pub id: String,
    /// Name of the tool
    pub name: String,
    /// Arguments passed to the tool
    pub arguments: Value,
    /// Result from the tool
    pub result: Value,
    /// Whether execution was successful
    pub success: bool,
    /// Duration in milliseconds
    pub duration_ms: u64,
}

/// LLM response from a generation request
#[derive(Debug, Clone)]
pub struct LLMResponse {
    /// Text content of the response
    pub content: String,
    /// Tool calls requested by the model
    pub tool_calls: Vec<ToolCallRequest>,
    /// Reason the response finished
    pub finish_reason: String,
}

/// Result from a complete agent execution
#[derive(Debug)]
pub struct AgentResponse {
    /// Final text response
    pub content: String,
    /// All tool calls made during execution
    pub tool_calls: Vec<ToolCallRecord>,
    /// Number of iterations taken
    pub iterations: usize,
    /// Total tokens used (if available)
    pub tokens_used: Option<u64>,
}

/// Callback for receiving streaming tokens
pub type TokenCallback = Box<dyn Fn(&str) + Send + Sync>;

/// Callback for receiving tool call updates
pub type ToolCallback = Box<dyn Fn(&ToolCallRecord) + Send + Sync>;

/// The main Pawan agent
pub struct PawanAgent {
    /// Configuration
    config: PawanConfig,
    /// Tool registry
    tools: ToolRegistry,
    /// Conversation history
    history: Vec<Message>,
    /// System prompt
    system_prompt: String,
    /// Workspace root
    workspace_root: PathBuf,
    /// HTTP client for API calls
    http_client: reqwest::Client,
    /// Ollama base URL
    ollama_url: String,
}

impl PawanAgent {
    /// Create a new PawanAgent
    pub fn new(config: PawanConfig, workspace_root: PathBuf) -> Self {
        let tools = ToolRegistry::with_defaults(workspace_root.clone());
        let system_prompt = config.get_system_prompt();

        Self {
            config,
            tools,
            history: Vec::new(),
            system_prompt,
            workspace_root,
            http_client: reqwest::Client::new(),
            ollama_url: std::env::var("OLLAMA_URL")
                .unwrap_or_else(|_| "http://localhost:11434".to_string()),
        }
    }

    /// Create with a specific tool registry
    pub fn with_tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    /// Set the system prompt
    pub fn with_system_prompt(mut self, prompt: String) -> Self {
        self.system_prompt = prompt;
        self
    }

    /// Get the current conversation history
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// Get the configuration
    pub fn config(&self) -> &PawanConfig {
        &self.config
    }

    /// Clear the conversation history
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Add a message to history
    pub fn add_message(&mut self, message: Message) {
        self.history.push(message);
    }

    /// Get tool definitions for the LLM
    pub fn get_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools.get_definitions()
    }

    /// Execute a single prompt with tool calling support
    pub async fn execute(&mut self, user_prompt: &str) -> Result<AgentResponse> {
        self.execute_with_callbacks(user_prompt, None, None).await
    }

    /// Execute with optional callbacks for streaming
    pub async fn execute_with_callbacks(
        &mut self,
        user_prompt: &str,
        on_token: Option<TokenCallback>,
        on_tool: Option<ToolCallback>,
    ) -> Result<AgentResponse> {
        // Add user message to history
        self.history.push(Message {
            role: Role::User,
            content: user_prompt.to_string(),
            tool_calls: vec![],
            tool_result: None,
        });

        let mut all_tool_calls = Vec::new();
        let mut iterations = 0;
        let max_iterations = self.config.max_tool_iterations;

        loop {
            iterations += 1;
            if iterations > max_iterations {
                return Err(PawanError::Agent(format!(
                    "Max tool iterations ({}) exceeded",
                    max_iterations
                )));
            }

            // Generate response from LLM
            let response = self.generate_with_tools(&on_token).await?;

            // Check if we have tool calls
            if response.tool_calls.is_empty() {
                // No tool calls, we're done
                self.history.push(Message {
                    role: Role::Assistant,
                    content: response.content.clone(),
                    tool_calls: vec![],
                    tool_result: None,
                });

                return Ok(AgentResponse {
                    content: response.content,
                    tool_calls: all_tool_calls,
                    iterations,
                    tokens_used: None,
                });
            }

            // Add assistant message with tool calls
            self.history.push(Message {
                role: Role::Assistant,
                content: response.content.clone(),
                tool_calls: response.tool_calls.clone(),
                tool_result: None,
            });

            // Execute tool calls
            for tool_call in &response.tool_calls {
                let start = std::time::Instant::now();

                let result = self
                    .tools
                    .execute(&tool_call.name, tool_call.arguments.clone())
                    .await;

                let duration_ms = start.elapsed().as_millis() as u64;

                let (result_value, success) = match result {
                    Ok(v) => (v, true),
                    Err(e) => (json!({"error": e.to_string()}), false),
                };

                let record = ToolCallRecord {
                    id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    arguments: tool_call.arguments.clone(),
                    result: result_value.clone(),
                    success,
                    duration_ms,
                };

                // Notify callback
                if let Some(ref callback) = on_tool {
                    callback(&record);
                }

                all_tool_calls.push(record);

                // Add tool result to history
                self.history.push(Message {
                    role: Role::Tool,
                    content: serde_json::to_string(&result_value).unwrap_or_default(),
                    tool_calls: vec![],
                    tool_result: Some(ToolResultMessage {
                        tool_call_id: tool_call.id.clone(),
                        content: result_value,
                        success,
                    }),
                });
            }
        }
    }

    /// Generate a response with tool calling support using Ollama API
    async fn generate_with_tools(&self, on_token: &Option<TokenCallback>) -> Result<LLMResponse> {
        let tool_defs = self.get_tool_definitions();

        // Build messages for Ollama
        let mut messages = vec![json!({
            "role": "system",
            "content": self.system_prompt
        })];

        for msg in &self.history {
            match msg.role {
                Role::System => {
                    messages.push(json!({
                        "role": "system",
                        "content": msg.content
                    }));
                }
                Role::User => {
                    messages.push(json!({
                        "role": "user",
                        "content": msg.content
                    }));
                }
                Role::Assistant => {
                    if msg.tool_calls.is_empty() {
                        messages.push(json!({
                            "role": "assistant",
                            "content": msg.content
                        }));
                    } else {
                        // Assistant with tool calls
                        let tool_calls: Vec<Value> = msg
                            .tool_calls
                            .iter()
                            .map(|tc| {
                                json!({
                                    "function": {
                                        "name": tc.name,
                                        "arguments": tc.arguments
                                    }
                                })
                            })
                            .collect();

                        messages.push(json!({
                            "role": "assistant",
                            "content": msg.content,
                            "tool_calls": tool_calls
                        }));
                    }
                }
                Role::Tool => {
                    if let Some(ref tool_result) = msg.tool_result {
                        messages.push(json!({
                            "role": "tool",
                            "content": serde_json::to_string(&tool_result.content)
                                .unwrap_or_else(|_| tool_result.content.to_string())
                        }));
                    }
                }
            }
        }

        // Build tools array for Ollama
        let tools: Vec<Value> = tool_defs
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters
                    }
                })
            })
            .collect();

        // Make request to Ollama
        let request_body = json!({
            "model": self.config.model,
            "messages": messages,
            "tools": tools,
            "stream": on_token.is_some(),
            "options": {
                "temperature": self.config.temperature
            }
        });

        let url = format!("{}/api/chat", self.ollama_url);

        if on_token.is_some() {
            // Streaming response
            self.generate_streaming(&url, request_body, on_token).await
        } else {
            // Non-streaming response
            self.generate_non_streaming(&url, request_body).await
        }
    }

    /// Non-streaming generation
    async fn generate_non_streaming(&self, url: &str, body: Value) -> Result<LLMResponse> {
        let response = self
            .http_client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| PawanError::Llm(format!("HTTP request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(PawanError::Llm(format!(
                "Ollama request failed ({}): {}",
                status, text
            )));
        }

        let response_json: Value = response
            .json()
            .await
            .map_err(|e| PawanError::Llm(format!("Failed to parse response: {}", e)))?;

        self.parse_ollama_response(&response_json)
    }

    /// Streaming generation
    async fn generate_streaming(
        &self,
        url: &str,
        body: Value,
        on_token: &Option<TokenCallback>,
    ) -> Result<LLMResponse> {
        let response = self
            .http_client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| PawanError::Llm(format!("HTTP request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(PawanError::Llm(format!(
                "Ollama request failed ({}): {}",
                status, text
            )));
        }

        let mut content = String::new();
        let mut tool_calls = Vec::new();
        let mut finish_reason = "stop".to_string();

        // Read streaming response
        let mut stream = response.bytes_stream();
        use futures::StreamExt;

        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| PawanError::Llm(format!("Stream error: {}", e)))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // Process complete JSON lines
            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].to_string();
                buffer = buffer[newline_pos + 1..].to_string();

                if line.trim().is_empty() {
                    continue;
                }

                if let Ok(json) = serde_json::from_str::<Value>(&line) {
                    // Extract content from message
                    if let Some(msg) = json.get("message") {
                        if let Some(c) = msg.get("content").and_then(|v| v.as_str()) {
                            if let Some(ref callback) = on_token {
                                callback(c);
                            }
                            content.push_str(c);
                        }

                        // Check for tool calls
                        if let Some(tc_array) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                            for tc in tc_array {
                                if let Some(func) = tc.get("function") {
                                    let name = func
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let arguments =
                                        func.get("arguments").cloned().unwrap_or(json!({}));

                                    tool_calls.push(ToolCallRequest {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        name,
                                        arguments,
                                    });
                                }
                            }
                            finish_reason = "tool_calls".to_string();
                        }
                    }

                    // Check if done
                    if json.get("done").and_then(|v| v.as_bool()).unwrap_or(false) {
                        break;
                    }
                }
            }
        }

        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason,
        })
    }

    /// Parse Ollama response JSON
    fn parse_ollama_response(&self, json: &Value) -> Result<LLMResponse> {
        let message = json
            .get("message")
            .ok_or_else(|| PawanError::Llm("No message in response".into()))?;

        let content = message
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mut tool_calls = Vec::new();
        let mut finish_reason = "stop".to_string();

        // Parse tool calls if present
        if let Some(tc_array) = message.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tc_array {
                if let Some(func) = tc.get("function") {
                    let name = func
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments = func.get("arguments").cloned().unwrap_or(json!({}));

                    tool_calls.push(ToolCallRequest {
                        id: uuid::Uuid::new_v4().to_string(),
                        name,
                        arguments,
                    });
                }
            }
            finish_reason = "tool_calls".to_string();
        }

        // Check done_reason if available
        if let Some(reason) = json.get("done_reason").and_then(|v| v.as_str()) {
            finish_reason = reason.to_string();
        }

        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason,
        })
    }

    /// Execute a healing task (fix compilation errors, warnings, etc.)
    pub async fn heal(&mut self) -> Result<AgentResponse> {
        let prompt = format!(
            r#"I need you to heal this Rust project. Please:

1. Run `cargo check` to see any compilation errors
2. If there are errors, analyze them and fix them one at a time
3. Run `cargo clippy` to check for warnings
4. Fix any warnings that are reasonable to fix
5. Run `cargo test` to verify tests pass
6. Report what you fixed

The workspace is at: {}

Please proceed step by step, verifying each fix compiles before moving on."#,
            self.workspace_root.display()
        );

        self.execute(&prompt).await
    }

    /// Execute a task with a specific prompt
    pub async fn task(&mut self, task_description: &str) -> Result<AgentResponse> {
        let prompt = format!(
            r#"I need you to complete the following coding task:

{}

The workspace is at: {}

Please:
1. First explore the codebase to understand the relevant code
2. Make the necessary changes
3. Verify the changes compile with `cargo check`
4. Run relevant tests if applicable

Explain your changes as you go."#,
            task_description,
            self.workspace_root.display()
        );

        self.execute(&prompt).await
    }

    /// Generate a commit message for current changes
    pub async fn generate_commit_message(&mut self) -> Result<String> {
        let prompt = r#"Please:
1. Run `git status` to see what files are changed
2. Run `git diff --cached` to see staged changes (or `git diff` for unstaged)
3. Generate a concise, descriptive commit message following conventional commits format

Only output the suggested commit message, nothing else."#;

        let response = self.execute(prompt).await?;
        Ok(response.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_serialization() {
        let msg = Message {
            role: Role::User,
            content: "Hello".to_string(),
            tool_calls: vec![],
            tool_result: None,
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("user"));
        assert!(json.contains("Hello"));
    }

    #[test]
    fn test_tool_call_request() {
        let tc = ToolCallRequest {
            id: "123".to_string(),
            name: "read_file".to_string(),
            arguments: json!({"path": "test.txt"}),
        };

        let json = serde_json::to_string(&tc).unwrap();
        assert!(json.contains("read_file"));
        assert!(json.contains("test.txt"));
    }
}
