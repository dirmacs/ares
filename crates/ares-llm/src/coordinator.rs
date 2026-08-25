//! Generic Tool Coordinator for Multi-Turn Tool Calling
//!
//! This module provides a provider-agnostic `ToolCoordinator` that works with any
//! `LLMClient` implementation. It handles the complete tool calling loop:
//!
//! 1. Send prompt with available tools to the LLM
//! 2. If the model requests tool calls, execute them
//! 3. Send tool results back to the model  
//! 4. Repeat until completion or max iterations
//!
//! # Example
//!
//! ```rust,ignore
//! use ares_llm::coordinator::{ToolCoordinator, ToolCallingConfig};
//! use ares_llm::Provider;
//! use ares_tools::{Tools, Tool};
//! use cordis::Context;
//! use std::sync::Arc;
//!
//! let client = Provider::from_env()?.create_client().await?;
//! let tools = Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new()));
//! let coordinator = ToolCoordinator::new(client, tools, ToolCallingConfig::default());
//! let ctx = Context::new_root();
//!
//! let result = coordinator.execute(
//!     Some("You are a helpful assistant."),
//!     "What's 2 + 2?",
//!     &ctx,
//! ).await?;
//!
//! println!("Response: {}", result.content);
//! println!("Tool calls made: {}", result.tool_calls.len());
//! ```

use crate::capabilities::{CapabilityRequirements, ModelCapabilities};
use crate::client::{LLMClient, TokenUsage};
#[cfg(test)]
use ares_tools::Tool;
use ares_tools::Tools;
use ares_types::types::{Result, ToolCall};
use cordis::Context;
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::timeout;

use serde_json::Value;
use std::collections::HashSet;
use std::fmt;

// Provider dispatch coordination (R42)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LoadBalanceStrategy {
    #[default]
    RoundRobin,
    LeastLoaded,
    Affinity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderEndpoint {
    pub id: String,
    pub model: String,
    pub capabilities: ModelCapabilities,
    #[serde(default)]
    pub in_flight_requests: u32,
    #[serde(default)]
    pub affinity_group: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorConfig {
    pub providers: Vec<ProviderEndpoint>,
    #[serde(default)]
    pub fallback_chain: Vec<String>,
    #[serde(default)]
    pub load_balance: LoadBalanceStrategy,
    #[serde(default)]
    pub requirements: CapabilityRequirements,
    #[serde(default)]
    pub affinity_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteDecision {
    pub provider_id: String,
    pub model: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchPlan {
    pub primary: RouteDecision,
    pub fallbacks: Vec<RouteDecision>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DispatchState {
    pub round_robin_cursor: usize,
    pub session_affinity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordinatorDispatchError {
    NoAvailableProvider(String),
    AllFailed(Vec<String>),
    InvalidConfig(String),
}

impl fmt::Display for CoordinatorDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAvailableProvider(msg) => write!(f, "no available provider: {msg}"),
            Self::AllFailed(errors) => write!(
                f,
                "all providers failed ({} attempts): {}",
                errors.len(),
                errors.join("; ")
            ),
            Self::InvalidConfig(msg) => write!(f, "invalid coordinator config: {msg}"),
        }
    }
}

impl std::error::Error for CoordinatorDispatchError {}

impl fmt::Display for RouteDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "route {} ({}) — {}",
            self.provider_id, self.model, self.reason
        )
    }
}

impl fmt::Display for DispatchPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dispatch primary={}", self.primary)?;
        if !self.fallbacks.is_empty() {
            let ids: Vec<_> = self
                .fallbacks
                .iter()
                .map(|r| r.provider_id.as_str())
                .collect();
            write!(f, ", fallbacks=[{}]", ids.join(", "))?;
        }
        Ok(())
    }
}

impl fmt::Display for LoadBalanceStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RoundRobin => write!(f, "round_robin"),
            Self::LeastLoaded => write!(f, "least_loaded"),
            Self::Affinity => write!(f, "affinity"),
        }
    }
}

pub fn validate_coordinator_config(
    config: &CoordinatorConfig,
) -> std::result::Result<(), CoordinatorDispatchError> {
    if config.providers.is_empty() {
        return Err(CoordinatorDispatchError::InvalidConfig(
            "providers list must not be empty".to_string(),
        ));
    }
    let mut seen = HashSet::new();
    for provider in &config.providers {
        if !seen.insert(provider.id.clone()) {
            return Err(CoordinatorDispatchError::InvalidConfig(format!(
                "duplicate provider id '{}'",
                provider.id
            )));
        }
        if provider.id.trim().is_empty() {
            return Err(CoordinatorDispatchError::InvalidConfig(
                "provider id must not be empty".to_string(),
            ));
        }
    }
    for fallback_id in &config.fallback_chain {
        if !config.providers.iter().any(|p| &p.id == fallback_id) {
            return Err(CoordinatorDispatchError::InvalidConfig(format!(
                "fallback_chain references unknown provider '{fallback_id}'"
            )));
        }
    }
    Ok(())
}

fn eligible_providers<'a>(
    config: &'a CoordinatorConfig,
    exclude: &HashSet<&str>,
) -> Vec<&'a ProviderEndpoint> {
    let mut eligible: Vec<_> = config
        .providers
        .iter()
        .filter(|p| !exclude.contains(p.id.as_str()))
        .filter(|p| p.capabilities.satisfies(&config.requirements))
        .collect();
    eligible.sort_by(|a, b| {
        let score_a = a.capabilities.score(&config.requirements);
        let score_b = b.capabilities.score(&config.requirements);
        score_b.cmp(&score_a).then_with(|| a.id.cmp(&b.id))
    });
    eligible
}

fn pick_by_strategy<'a>(
    config: &CoordinatorConfig,
    state: &mut DispatchState,
    eligible: &'a [&'a ProviderEndpoint],
) -> &'a ProviderEndpoint {
    match config.load_balance {
        LoadBalanceStrategy::RoundRobin => {
            let idx = state.round_robin_cursor % eligible.len();
            state.round_robin_cursor = state.round_robin_cursor.saturating_add(1);
            eligible[idx]
        }
        LoadBalanceStrategy::LeastLoaded => eligible
            .iter()
            .copied()
            .min_by_key(|p| (p.in_flight_requests, p.id.as_str()))
            .expect("eligible is non-empty"),
        LoadBalanceStrategy::Affinity => {
            let key = state
                .session_affinity
                .as_deref()
                .or(config.affinity_key.as_deref());
            if let Some(affinity) = key {
                if let Some(match_provider) = eligible
                    .iter()
                    .copied()
                    .find(|p| p.affinity_group.as_deref() == Some(affinity))
                {
                    return match_provider;
                }
            }
            eligible[0]
        }
    }
}

fn endpoint_to_decision(endpoint: &ProviderEndpoint, reason: impl Into<String>) -> RouteDecision {
    RouteDecision {
        provider_id: endpoint.id.clone(),
        model: endpoint.model.clone(),
        reason: reason.into(),
    }
}

pub fn route_request(
    config: &CoordinatorConfig,
    state: &mut DispatchState,
    exclude: &[&str],
) -> std::result::Result<RouteDecision, CoordinatorDispatchError> {
    validate_coordinator_config(config)?;
    let exclude_set: HashSet<&str> = exclude.iter().copied().collect();
    let eligible = eligible_providers(config, &exclude_set);
    if eligible.is_empty() {
        return Err(CoordinatorDispatchError::NoAvailableProvider(
            "no provider satisfies requirements".to_string(),
        ));
    }
    let selected = pick_by_strategy(config, state, &eligible);
    let reason = format!(
        "selected via {} (score {})",
        config.load_balance,
        selected.capabilities.score(&config.requirements)
    );
    Ok(endpoint_to_decision(selected, reason))
}

pub fn select_fallback(
    config: &CoordinatorConfig,
    plan: &DispatchPlan,
    failed_provider_id: &str,
    attempt_errors: &[String],
) -> std::result::Result<Option<RouteDecision>, CoordinatorDispatchError> {
    validate_coordinator_config(config)?;
    let mut tried: HashSet<&str> = HashSet::new();
    tried.insert(plan.primary.provider_id.as_str());
    for fb in &plan.fallbacks {
        tried.insert(fb.provider_id.as_str());
    }
    tried.insert(failed_provider_id);
    for fallback_id in &config.fallback_chain {
        if tried.contains(fallback_id.as_str()) {
            continue;
        }
        let Some(endpoint) = config.providers.iter().find(|p| &p.id == fallback_id) else {
            continue;
        };
        if !endpoint.capabilities.satisfies(&config.requirements) {
            continue;
        }
        return Ok(Some(endpoint_to_decision(
            endpoint,
            format!("fallback after failure of '{failed_provider_id}'"),
        )));
    }
    let remaining: Vec<_> = config
        .providers
        .iter()
        .filter(|p| !tried.contains(p.id.as_str()))
        .filter(|p| p.capabilities.satisfies(&config.requirements))
        .collect();
    if let Some(endpoint) = remaining.first() {
        return Ok(Some(endpoint_to_decision(
            endpoint,
            format!("secondary fallback after '{failed_provider_id}'"),
        )));
    }
    if attempt_errors.is_empty() {
        Ok(None)
    } else {
        Err(CoordinatorDispatchError::AllFailed(attempt_errors.to_vec()))
    }
}

pub fn parse_coordinator_response(
    payload: &str,
) -> std::result::Result<DispatchPlan, CoordinatorDispatchError> {
    let value: Value = serde_json::from_str(payload)
        .map_err(|e| CoordinatorDispatchError::InvalidConfig(format!("invalid JSON: {e}")))?;
    let primary_value = value.get("primary").ok_or_else(|| {
        CoordinatorDispatchError::InvalidConfig("missing 'primary' field".to_string())
    })?;
    let primary: RouteDecision = serde_json::from_value(primary_value.clone()).map_err(|e| {
        CoordinatorDispatchError::InvalidConfig(format!("invalid primary route: {e}"))
    })?;
    let fallbacks = match value.get("fallbacks") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                serde_json::from_value(item.clone()).map_err(|e| {
                    CoordinatorDispatchError::InvalidConfig(format!("invalid fallback route: {e}"))
                })
            })
            .collect::<std::result::Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(CoordinatorDispatchError::InvalidConfig(
                "'fallbacks' must be an array".to_string(),
            ));
        }
        None => Vec::new(),
    };
    if primary.provider_id.trim().is_empty() {
        return Err(CoordinatorDispatchError::InvalidConfig(
            "primary provider_id must not be empty".to_string(),
        ));
    }
    Ok(DispatchPlan { primary, fallbacks })
}

pub fn build_dispatch_plan(
    config: &CoordinatorConfig,
    state: &mut DispatchState,
) -> std::result::Result<DispatchPlan, CoordinatorDispatchError> {
    validate_coordinator_config(config)?;
    let primary = route_request(config, state, &[])?;
    let mut fallbacks = Vec::new();
    let mut failed_id = primary.provider_id.clone();
    loop {
        let plan_snapshot = DispatchPlan {
            primary: primary.clone(),
            fallbacks: fallbacks.clone(),
        };
        match select_fallback(config, &plan_snapshot, &failed_id, &[]) {
            Ok(Some(next)) => {
                if fallbacks.iter().any(|f| f.provider_id == next.provider_id) {
                    break;
                }
                failed_id = next.provider_id.clone();
                fallbacks.push(next);
            }
            Ok(None) => break,
            Err(e) => return Err(e),
        }
        if !config.fallback_chain.is_empty() && fallbacks.len() >= config.fallback_chain.len() {
            break;
        }
    }
    Ok(DispatchPlan { primary, fallbacks })
}

/// Configuration for tool calling coordination behavior.
///
/// Controls how the coordinator handles multi-turn tool calling,
/// including iteration limits, parallelism, and timeout settings.
#[derive(Debug, Clone)]
pub struct ToolCallingConfig {
    /// Maximum number of LLM iterations (not tool calls) before stopping.
    /// Each iteration is one round-trip to the LLM.
    pub max_iterations: usize,

    /// Whether to execute multiple tool calls in parallel.
    /// When false, tools are executed sequentially.
    pub parallel_execution: bool,

    /// Timeout for individual tool execution.
    pub tool_timeout: Duration,

    /// Whether to include tool results in the final response context.
    pub include_tool_results: bool,

    /// Whether to stop on the first tool error, or continue with remaining tools.
    pub stop_on_error: bool,
}

impl Default for ToolCallingConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            parallel_execution: true,
            tool_timeout: Duration::from_secs(30),
            include_tool_results: true,
            stop_on_error: false,
        }
    }
}

/// Record of a single tool call execution.
///
/// Captures all details about a tool invocation including timing,
/// success status, and any errors that occurred.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Unique identifier for this tool call (from the LLM).
    pub id: String,
    /// Name of the tool that was called.
    pub name: String,
    /// Arguments passed to the tool.
    pub arguments: serde_json::Value,
    /// Result returned by the tool (or error object).
    pub result: serde_json::Value,
    /// Whether the tool execution was successful.
    pub success: bool,
    /// Time taken to execute the tool in milliseconds.
    pub duration_ms: u64,
    /// Error message if the tool failed.
    pub error: Option<String>,
}

/// Reason why a tool coordination session ended.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FinishReason {
    /// Model decided to stop (no more tool calls).
    Stop,
    /// Hit the maximum iterations limit.
    MaxIterations,
    /// An unrecoverable error occurred.
    Error(String),
    /// Model tried to call an unknown tool.
    UnknownTool(String),
}

impl std::fmt::Display for FinishReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FinishReason::Stop => write!(f, "stop"),
            FinishReason::MaxIterations => write!(f, "max_iterations"),
            FinishReason::Error(e) => write!(f, "error: {}", e),
            FinishReason::UnknownTool(t) => write!(f, "unknown_tool: {}", t),
        }
    }
}

/// A message in a tool-calling conversation.
///
/// Represents all message types that can appear in a multi-turn
/// conversation with tool calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    /// The role of the message sender.
    pub role: MessageRole,
    /// The text content of the message.
    pub content: String,
    /// Tool calls requested by the assistant (only for Assistant role).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Tool result content (only for Tool role).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Role of a message sender in a tool-calling conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// System instructions.
    System,
    /// User message.
    User,
    /// Assistant response.
    Assistant,
    /// Tool execution result.
    Tool,
}

impl ConversationMessage {
    /// Create a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Create a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Create an assistant message with optional tool calls.
    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
        }
    }

    /// Create a tool result message.
    pub fn tool_result(tool_call_id: impl Into<String>, result: &serde_json::Value) -> Self {
        Self {
            role: MessageRole::Tool,
            content: serde_json::to_string(result).unwrap_or_else(|_| "{}".to_string()),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    /// Convert to the simple (role, content) format for LLMClient::generate_with_history.
    pub fn to_role_content(&self) -> (String, String) {
        let role = match self.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        (role.to_string(), self.content.clone())
    }
}

/// Result of a complete tool coordination session.
///
/// Contains all information about what happened during the multi-turn
/// conversation, including the final response, all tool calls made,
/// token usage, and message history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorResult {
    /// Final text response from the model.
    pub content: String,

    /// All tool calls made during the session.
    pub tool_calls: Vec<ToolCallRecord>,

    /// Number of LLM iterations (round-trips) performed.
    pub iterations: usize,

    /// Why the session ended.
    pub finish_reason: FinishReason,

    /// Accumulated token usage across all iterations.
    pub total_usage: TokenUsage,

    /// Full message history (useful for debugging and training data).
    pub message_history: Vec<ConversationMessage>,
}

/// Generic tool coordinator that works with any LLMClient.
///
/// Manages multi-turn tool calling conversations by:
/// 1. Sending prompts with tool definitions to the LLM
/// 2. Parsing tool call requests from the response
/// 3. Executing tools and collecting results
/// 4. Sending results back to the LLM
/// 5. Repeating until the LLM produces a final response
///
/// # Type Parameters
///
/// The coordinator is generic over the LLMClient, but typically you'll use
/// it with `Box<dyn LLMClient>` for maximum flexibility.
pub struct ToolCoordinator {
    client: Box<dyn LLMClient>,
    /// Ordered fallback chain: `(provider_name, client)` pairs tried when
    /// the primary fails with a retryable error.
    #[allow(dead_code)]
    fallback_chain: Vec<(String, Box<dyn LLMClient>)>,
    tools: Arc<Tools>,
    config: ToolCallingConfig,
    observability: Option<Arc<dyn crate::observability::ObservabilitySink>>,
}

impl ToolCoordinator {
    /// Create a new ToolCoordinator with the given client, tools, and config.
    pub fn new(client: Box<dyn LLMClient>, tools: Arc<Tools>, config: ToolCallingConfig) -> Self {
        Self {
            client,
            fallback_chain: Vec::new(),
            tools,
            config,
            observability: None,
        }
    }

    /// Create a new ToolCoordinator with default configuration.
    pub fn with_defaults(client: Box<dyn LLMClient>, tools: Arc<Tools>) -> Self {
        Self::new(client, tools, ToolCallingConfig::default())
    }

    /// Create a new ToolCoordinator with a fallback chain.
    pub fn with_fallbacks(
        client: Box<dyn LLMClient>,
        fallback_chain: Vec<(String, Box<dyn LLMClient>)>,
        tools: Arc<Tools>,
        config: ToolCallingConfig,
    ) -> Self {
        Self {
            client,
            fallback_chain,
            tools,
            config,
            observability: None,
        }
    }

    /// Attach an observability sink to this coordinator.
    pub fn with_observability(
        mut self,
        obs: Arc<dyn crate::observability::ObservabilitySink>,
    ) -> Self {
        self.observability = Some(obs);
        self
    }

    /// Execute a complete tool-calling conversation loop.
    ///
    /// This method handles the full tool calling loop:
    /// 1. Send the initial prompt with available tools
    /// 2. If the model requests tool calls, execute them
    /// 3. Send tool results back to the model
    /// 4. Repeat until the model produces a final response or max iterations reached
    ///
    /// # Arguments
    ///
    /// * `system` - Optional system prompt
    /// * `prompt` - The user's prompt
    /// * `ctx` - Cordis context for `Tools::list` / `Tools::resolve` tenant derivation
    ///
    /// # Returns
    ///
    /// A `CoordinatorResult` containing the final response, all tool calls made,
    /// and execution metadata.
    pub async fn execute(
        &self,
        system: Option<&str>,
        prompt: &str,
        ctx: &Arc<Context>,
    ) -> Result<CoordinatorResult> {
        let tools = self.tools.list(ctx);
        let mut messages: Vec<ConversationMessage> = Vec::new();
        let mut all_tool_calls: Vec<ToolCallRecord> = Vec::new();
        let mut total_usage = TokenUsage::default();

        // Add system message if provided
        if let Some(sys) = system {
            messages.push(ConversationMessage::system(sys));
        }

        // Add user message
        messages.push(ConversationMessage::user(prompt));

        for iteration in 0..self.config.max_iterations {
            // Call LLM with tools
            let llm_start = Instant::now();
            let response = self
                .client
                .generate_with_tools_and_history(&messages, &tools)
                .await?;
            let llm_latency = llm_start.elapsed().as_millis() as i64;

            // Log the LLM call
            if let Some(obs) = &self.observability {
                let prompt_tok = response
                    .usage
                    .as_ref()
                    .map(|u| u.prompt_tokens as i64)
                    .unwrap_or(0);
                let completion_tok = response
                    .usage
                    .as_ref()
                    .map(|u| u.completion_tokens as i64)
                    .unwrap_or(0);
                let record = crate::observability::LlmCallRecord {
                    step_index: iteration as i32,
                    provider: "unknown".to_string(),
                    model: "unknown".to_string(),
                    prompt_tokens: prompt_tok,
                    completion_tokens: completion_tok,
                    latency_ms: llm_latency,
                    status: "success".to_string(),
                    cached_tokens: response.usage.as_ref().and_then(|u| u.cached_tokens),
                    total_time_ms: Some(llm_latency),
                };
                let _ = obs.log_llm_call(record).await;
            }

            // Accumulate usage
            if let Some(usage) = &response.usage {
                total_usage = TokenUsage::new(
                    total_usage.prompt_tokens + usage.prompt_tokens,
                    total_usage.completion_tokens + usage.completion_tokens,
                );
            }

            // Add assistant message to history
            messages.push(ConversationMessage::assistant(
                &response.content,
                response.tool_calls.clone(),
            ));

            // Check if we're done (no tool calls)
            if response.tool_calls.is_empty() {
                return Ok(CoordinatorResult {
                    content: response.content,
                    tool_calls: all_tool_calls,
                    iterations: iteration + 1,
                    finish_reason: FinishReason::Stop,
                    total_usage,
                    message_history: messages,
                });
            }

            // Validate that all requested tools exist
            for tool_call in &response.tool_calls {
                if self.tools.resolve(ctx, &tool_call.name).is_none() {
                    return Ok(CoordinatorResult {
                        content: response.content,
                        tool_calls: all_tool_calls,
                        iterations: iteration + 1,
                        finish_reason: FinishReason::UnknownTool(tool_call.name.clone()),
                        total_usage,
                        message_history: messages,
                    });
                }
            }

            // Execute tool calls
            let tool_start = Instant::now();
            let tool_results = self.execute_tool_calls(ctx, &response.tool_calls).await?;
            let tool_latency = tool_start.elapsed().as_millis() as i64;

            // Record tool calls and add results to message history
            for record in tool_results.into_iter() {
                // Log the tool call
                if let Some(obs) = &self.observability {
                    let status = if record.success {
                        "success".to_string()
                    } else {
                        "error".to_string()
                    };
                    let tool_record = crate::observability::ToolCallRecord {
                        step_index: iteration as i32,
                        tool_name: record.name.clone(),
                        tool_type: "builtin".to_string(),
                        arguments: record.arguments.clone(),
                        result: Some(record.result.clone()),
                        latency_ms: tool_latency,
                        status,
                    };
                    let _ = obs.log_tool_call(tool_record).await;
                }

                // Add tool result to messages
                messages.push(ConversationMessage::tool_result(&record.id, &record.result));
                all_tool_calls.push(record);
            }
        }

        // Hit max iterations
        Ok(CoordinatorResult {
            content: messages
                .last()
                .map(|m| m.content.clone())
                .unwrap_or_default(),
            tool_calls: all_tool_calls,
            iterations: self.config.max_iterations,
            finish_reason: FinishReason::MaxIterations,
            total_usage,
            message_history: messages,
        })
    }

    /// Execute tool calls, either in parallel or sequentially based on config.
    async fn execute_tool_calls(
        &self,
        ctx: &Arc<Context>,
        calls: &[ToolCall],
    ) -> Result<Vec<ToolCallRecord>> {
        if self.config.parallel_execution {
            self.execute_parallel(ctx, calls).await
        } else {
            self.execute_sequential(ctx, calls).await
        }
    }

    /// Execute tool calls in parallel.
    async fn execute_parallel(
        &self,
        ctx: &Arc<Context>,
        calls: &[ToolCall],
    ) -> Result<Vec<ToolCallRecord>> {
        let futures = calls.iter().map(|call| self.execute_single_tool(ctx, call));
        let results = join_all(futures).await;

        let mut records = Vec::with_capacity(results.len());
        for result in results {
            match result {
                Ok(record) => records.push(record),
                Err(e) if self.config.stop_on_error => return Err(e),
                Err(e) => {
                    // Create an error record for failed tools
                    records.push(ToolCallRecord {
                        id: "error".to_string(),
                        name: "unknown".to_string(),
                        arguments: serde_json::Value::Null,
                        result: serde_json::json!({"error": e.to_string()}),
                        success: false,
                        duration_ms: 0,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
        Ok(records)
    }

    /// Execute tool calls sequentially.
    async fn execute_sequential(
        &self,
        ctx: &Arc<Context>,
        calls: &[ToolCall],
    ) -> Result<Vec<ToolCallRecord>> {
        let mut records = Vec::with_capacity(calls.len());
        for call in calls {
            match self.execute_single_tool(ctx, call).await {
                Ok(record) => records.push(record),
                Err(e) if self.config.stop_on_error => return Err(e),
                Err(e) => {
                    records.push(ToolCallRecord {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                        result: serde_json::json!({"error": e.to_string()}),
                        success: false,
                        duration_ms: 0,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
        Ok(records)
    }

    /// Execute a single tool call with timeout.
    async fn execute_single_tool(
        &self,
        ctx: &Arc<Context>,
        call: &ToolCall,
    ) -> Result<ToolCallRecord> {
        let start = Instant::now();

        let result = timeout(
            self.config.tool_timeout,
            self.tools.execute(ctx, &call.name, call.arguments.clone()),
        )
        .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(value)) => Ok(ToolCallRecord {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                result: value,
                success: true,
                duration_ms,
                error: None,
            }),
            Ok(Err(e)) => Ok(ToolCallRecord {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                result: serde_json::json!({"error": e.to_string()}),
                success: false,
                duration_ms,
                error: Some(e.to_string()),
            }),
            Err(_) => Ok(ToolCallRecord {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                result: serde_json::json!({"error": "Tool execution timed out"}),
                success: false,
                duration_ms,
                error: Some("Tool execution timed out".to_string()),
            }),
        }
    }

    /// Get a reference to the underlying LLM client.
    pub fn client(&self) -> &dyn LLMClient {
        self.client.as_ref()
    }

    /// Get a reference to the Tools capability.
    pub fn tools(&self) -> &Arc<Tools> {
        &self.tools
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &ToolCallingConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::ModelCapabilities;
    use crate::client::{LLMClient, LLMResponse, TokenUsage};
    use ares_types::types::{Result, ToolCall, ToolDefinition};

    fn serde_roundtrip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).unwrap();
        let decoded: T = serde_json::from_str(&json).unwrap();
        assert_eq!(*value, decoded);
        decoded
    }

    #[test]
    fn test_tool_calling_config_default() {
        let config = ToolCallingConfig::default();
        assert_eq!(config.max_iterations, 10);
        assert!(config.parallel_execution);
        assert_eq!(config.tool_timeout, Duration::from_secs(30));
        assert!(config.include_tool_results);
        assert!(!config.stop_on_error);
    }

    #[test]
    fn test_tool_calling_config_clone() {
        let original = ToolCallingConfig::default();
        let mut cloned = original.clone();
        cloned.max_iterations = 2;
        cloned.parallel_execution = false;
        cloned.tool_timeout = Duration::from_secs(5);
        cloned.include_tool_results = false;
        cloned.stop_on_error = true;

        assert_eq!(original.max_iterations, 10);
        assert!(original.parallel_execution);
        assert_eq!(original.tool_timeout, Duration::from_secs(30));
        assert!(original.include_tool_results);
        assert!(!original.stop_on_error);

        assert_eq!(cloned.max_iterations, 2);
        assert!(!cloned.parallel_execution);
        assert_eq!(cloned.tool_timeout, Duration::from_secs(5));
        assert!(!cloned.include_tool_results);
        assert!(cloned.stop_on_error);
    }

    #[test]
    fn test_finish_reason_serialization() {
        for reason in [
            FinishReason::Stop,
            FinishReason::MaxIterations,
            FinishReason::Error("timeout".to_string()),
            FinishReason::UnknownTool("missing".to_string()),
        ] {
            serde_roundtrip(&reason);
        }
    }

    #[test]
    fn test_message_role_serialization() {
        for role in [
            MessageRole::System,
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::Tool,
        ] {
            let json = serde_json::to_string(&role).unwrap();
            assert!(json.chars().all(|c| !c.is_uppercase()));
            serde_roundtrip(&role);
        }
    }

    /// `CoordinatorResult` is the concrete session output type used by `execute`.
    type SessionResult = CoordinatorResult;

    #[test]
    fn test_coordinator_result_type_alias() {
        let result: SessionResult = CoordinatorResult {
            content: "All done".to_string(),
            tool_calls: vec![ToolCallRecord {
                id: "call_1".to_string(),
                name: "calculator".to_string(),
                arguments: serde_json::json!({"a": 1, "b": 1}),
                result: serde_json::json!({"sum": 2}),
                success: true,
                duration_ms: 12,
                error: None,
            }],
            iterations: 2,
            finish_reason: FinishReason::Stop,
            total_usage: TokenUsage::new(30, 15),
            message_history: vec![
                ConversationMessage::system("sys"),
                ConversationMessage::user("go"),
            ],
        };

        assert_eq!(result.content, "All done");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.iterations, 2);
        assert_eq!(result.finish_reason, FinishReason::Stop);
        assert_eq!(result.total_usage.prompt_tokens, 30);
    }

    #[test]
    fn test_coordinator_result_serde_roundtrip() {
        let result = CoordinatorResult {
            content: "done".to_string(),
            tool_calls: Vec::new(),
            iterations: 1,
            finish_reason: FinishReason::MaxIterations,
            total_usage: TokenUsage::default(),
            message_history: vec![ConversationMessage::user("ping")],
        };
        let json = serde_json::to_string(&result).unwrap();
        let decoded: CoordinatorResult = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.content, result.content);
        assert_eq!(decoded.tool_calls.len(), result.tool_calls.len());
        assert_eq!(decoded.iterations, result.iterations);
        assert_eq!(decoded.finish_reason, result.finish_reason);
        assert_eq!(decoded.total_usage, result.total_usage);
        assert_eq!(decoded.message_history.len(), result.message_history.len());
        assert_eq!(
            decoded.message_history[0].role,
            result.message_history[0].role
        );
    }

    #[test]
    fn test_conversation_message_serde_roundtrip() {
        let tool_calls = vec![ToolCall {
            id: "call_1".to_string(),
            name: "search".to_string(),
            arguments: serde_json::json!({"q": "ares"}),
        }];

        for msg in [
            ConversationMessage::system("system prompt"),
            ConversationMessage::user("hello"),
            ConversationMessage::assistant("thinking", tool_calls),
            ConversationMessage::tool_result("call_1", &serde_json::json!({"hits": 1})),
        ] {
            let json = serde_json::to_string(&msg).unwrap();
            let decoded: ConversationMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded.role, msg.role);
            assert_eq!(decoded.content, msg.content);
            assert_eq!(decoded.tool_calls.len(), msg.tool_calls.len());
            assert_eq!(decoded.tool_call_id, msg.tool_call_id);
        }
    }

    #[test]
    fn test_conversation_message_system() {
        let msg = ConversationMessage::system("You are a helpful assistant.");
        assert_eq!(msg.role, MessageRole::System);
        assert_eq!(msg.content, "You are a helpful assistant.");
        assert!(msg.tool_calls.is_empty());
        assert!(msg.tool_call_id.is_none());
    }

    #[test]
    fn test_conversation_message_user() {
        let msg = ConversationMessage::user("Hello!");
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.content, "Hello!");
    }

    #[test]
    fn test_conversation_message_assistant_with_tool_calls() {
        let tool_calls = vec![ToolCall {
            id: "call_1".to_string(),
            name: "calculator".to_string(),
            arguments: serde_json::json!({"a": 1, "b": 2}),
        }];
        let msg = ConversationMessage::assistant("Let me calculate that.", tool_calls.clone());
        assert_eq!(msg.role, MessageRole::Assistant);
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].name, "calculator");
    }

    #[test]
    fn test_conversation_message_tool_result() {
        let result = serde_json::json!({"result": 42});
        let msg = ConversationMessage::tool_result("call_1", &result);
        assert_eq!(msg.role, MessageRole::Tool);
        assert_eq!(msg.tool_call_id, Some("call_1".to_string()));
        assert!(msg.content.contains("42"));
    }

    #[test]
    fn test_finish_reason_display() {
        assert_eq!(FinishReason::Stop.to_string(), "stop");
        assert_eq!(FinishReason::MaxIterations.to_string(), "max_iterations");
        assert_eq!(
            FinishReason::Error("test error".to_string()).to_string(),
            "error: test error"
        );
        assert_eq!(
            FinishReason::UnknownTool("unknown".to_string()).to_string(),
            "unknown_tool: unknown"
        );
    }

    #[test]
    fn test_tool_call_record_serialization() {
        let record = ToolCallRecord {
            id: "call_1".to_string(),
            name: "test_tool".to_string(),
            arguments: serde_json::json!({"input": "test"}),
            result: serde_json::json!({"output": "result"}),
            success: true,
            duration_ms: 100,
            error: None,
        };

        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("test_tool"));
        assert!(json.contains("\"success\":true"));
    }

    struct MockToolFlowClient {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl MockToolFlowClient {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl LLMClient for MockToolFlowClient {
        async fn generate(&self, _prompt: &str) -> Result<String> {
            Ok(String::new())
        }

        async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
            Ok(String::new())
        }

        async fn generate_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: String::new(),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }

        async fn generate_with_tools(
            &self,
            _prompt: &str,
            _tools: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: String::new(),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }

        async fn generate_with_tools_and_history(
            &self,
            _messages: &[ConversationMessage],
            _tools: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                Ok(LLMResponse {
                    content: "Let me calculate".to_string(),
                    tool_calls: vec![ToolCall {
                        id: "call_1".to_string(),
                        name: "calculator".to_string(),
                        arguments: serde_json::json!({"operation": "add", "a": 2, "b": 2}),
                    }],
                    finish_reason: "tool_calls".to_string(),
                    usage: Some(TokenUsage::new(10, 5)),
                })
            } else {
                Ok(LLMResponse {
                    content: "The answer is 4".to_string(),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    usage: Some(TokenUsage::new(5, 3)),
                })
            }
        }

        async fn stream(
            &self,
            _prompt: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(ares_types::types::AppError::Internal("not used".into()))
        }

        async fn stream_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(ares_types::types::AppError::Internal("not used".into()))
        }

        async fn stream_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(ares_types::types::AppError::Internal("not used".into()))
        }

        fn model_name(&self) -> &str {
            "mock"
        }
    }

    #[tokio::test]
    async fn test_tool_calling_flow_with_mock() {
        use ares_tools::calculator::Calculator;
        use std::sync::Arc;

        let tools = Arc::new(Tools::from_static([Arc::new(Calculator) as Arc<dyn Tool>]));
        let ctx = Context::new_root();

        let coordinator = ToolCoordinator::new(
            Box::new(MockToolFlowClient::new()),
            tools,
            ToolCallingConfig::default(),
        );

        let result = coordinator
            .execute(None, "What is 2 + 2?", &ctx)
            .await
            .expect("coordinator should succeed");

        assert_eq!(result.finish_reason, FinishReason::Stop);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "calculator");
        assert!(result.tool_calls[0].success);
        assert_eq!(result.iterations, 2);
        assert!(result.content.contains('4'));
    }

    #[tokio::test]
    async fn test_tool_calling_unknown_tool_stops() {
        struct UnknownToolClient;

        #[async_trait::async_trait]
        impl LLMClient for UnknownToolClient {
            async fn generate(&self, _prompt: &str) -> Result<String> {
                Ok(String::new())
            }
            async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
                Ok(String::new())
            }
            async fn generate_with_history(
                &self,
                _messages: &[(String, String)],
            ) -> Result<LLMResponse> {
                Ok(LLMResponse {
                    content: String::new(),
                    tool_calls: vec![],
                    finish_reason: "stop".into(),
                    usage: None,
                })
            }
            async fn generate_with_tools(
                &self,
                _prompt: &str,
                _tools: &[ToolDefinition],
            ) -> Result<LLMResponse> {
                Ok(LLMResponse {
                    content: String::new(),
                    tool_calls: vec![],
                    finish_reason: "stop".into(),
                    usage: None,
                })
            }
            async fn generate_with_tools_and_history(
                &self,
                _messages: &[ConversationMessage],
                _tools: &[ToolDefinition],
            ) -> Result<LLMResponse> {
                Ok(LLMResponse {
                    content: "calling".into(),
                    tool_calls: vec![ToolCall {
                        id: "1".into(),
                        name: "missing_tool".into(),
                        arguments: serde_json::json!({}),
                    }],
                    finish_reason: "tool_calls".into(),
                    usage: None,
                })
            }
            async fn stream(
                &self,
                _prompt: &str,
            ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>
            {
                Err(ares_types::types::AppError::Internal("n/a".into()))
            }
            async fn stream_with_system(
                &self,
                _system: &str,
                _prompt: &str,
            ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>
            {
                Err(ares_types::types::AppError::Internal("n/a".into()))
            }
            async fn stream_with_history(
                &self,
                _messages: &[(String, String)],
            ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>
            {
                Err(ares_types::types::AppError::Internal("n/a".into()))
            }
            fn model_name(&self) -> &str {
                "mock"
            }
        }

        let tools = Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new()));
        let ctx = Context::new_root();
        let coordinator = ToolCoordinator::new(
            Box::new(UnknownToolClient),
            tools,
            ToolCallingConfig::default(),
        );
        let result = coordinator.execute(None, "go", &ctx).await.unwrap();
        assert!(matches!(result.finish_reason, FinishReason::UnknownTool(_)));
    }

    fn dispatch_endpoint(id: &str, model: &str, caps: ModelCapabilities) -> ProviderEndpoint {
        ProviderEndpoint {
            id: id.to_string(),
            model: model.to_string(),
            capabilities: caps,
            in_flight_requests: 0,
            affinity_group: None,
        }
    }

    fn dispatch_test_config(providers: Vec<ProviderEndpoint>) -> CoordinatorConfig {
        CoordinatorConfig {
            providers,
            fallback_chain: vec![],
            load_balance: LoadBalanceStrategy::RoundRobin,
            requirements: CapabilityRequirements::default(),
            affinity_key: None,
        }
    }

    #[test]
    fn dispatch_coordinator_config_serde_roundtrip() {
        let config = dispatch_test_config(vec![dispatch_endpoint(
            "openai",
            "gpt-4o",
            ModelCapabilities::for_model("gpt-4o"),
        )]);
        let json = serde_json::to_string(&config).unwrap();
        let decoded: CoordinatorConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.providers.len(), config.providers.len());
        assert_eq!(decoded.fallback_chain, config.fallback_chain);
        assert_eq!(decoded.load_balance, config.load_balance);
    }

    #[test]
    fn dispatch_plan_serde_roundtrip() {
        let plan = DispatchPlan {
            primary: RouteDecision {
                provider_id: "openai".into(),
                model: "gpt-4o".into(),
                reason: "primary".into(),
            },
            fallbacks: vec![RouteDecision {
                provider_id: "ollama".into(),
                model: "llama3".into(),
                reason: "fallback".into(),
            }],
        };
        serde_roundtrip(&plan);
    }

    #[test]
    fn dispatch_route_decision_serde_roundtrip() {
        let decision = RouteDecision {
            provider_id: "openai".into(),
            model: "gpt-4o".into(),
            reason: "best score".into(),
        };
        serde_roundtrip(&decision);
    }

    #[test]
    fn dispatch_load_balance_strategy_serde_roundtrip() {
        serde_roundtrip(&LoadBalanceStrategy::LeastLoaded);
        serde_roundtrip(&LoadBalanceStrategy::Affinity);
    }

    #[test]
    fn dispatch_route_request_picks_highest_scoring_provider() {
        let mut config = dispatch_test_config(vec![
            dispatch_endpoint("openai", "gpt-4o", ModelCapabilities::for_model("gpt-4o")),
            dispatch_endpoint("ollama", "llama3", ModelCapabilities::for_model("llama3")),
        ]);
        config.requirements = CapabilityRequirements::for_agent();
        let mut state = DispatchState::default();
        let route = route_request(&config, &mut state, &[]).unwrap();
        assert_eq!(route.provider_id, "openai");
    }

    #[test]
    fn dispatch_route_request_filters_by_capability_requirements() {
        let mut config = dispatch_test_config(vec![
            dispatch_endpoint(
                "basic",
                "basic",
                ModelCapabilities {
                    supports_tools: false,
                    production_ready: true,
                    ..Default::default()
                },
            ),
            dispatch_endpoint("openai", "gpt-4o", ModelCapabilities::for_model("gpt-4o")),
        ]);
        config.requirements = CapabilityRequirements::for_agent();
        let mut state = DispatchState::default();
        let route = route_request(&config, &mut state, &[]).unwrap();
        assert_eq!(route.provider_id, "openai");
    }

    #[test]
    fn dispatch_route_request_no_available_provider() {
        let mut config = dispatch_test_config(vec![dispatch_endpoint(
            "basic",
            "basic",
            ModelCapabilities {
                supports_tools: false,
                ..Default::default()
            },
        )]);
        config.requirements = CapabilityRequirements::for_agent();
        let mut state = DispatchState::default();
        let err = route_request(&config, &mut state, &[]).unwrap_err();
        assert!(matches!(
            err,
            CoordinatorDispatchError::NoAvailableProvider(_)
        ));
    }

    #[test]
    fn dispatch_route_request_excludes_providers() {
        let config = dispatch_test_config(vec![
            dispatch_endpoint("openai", "gpt-4o", ModelCapabilities::for_model("gpt-4o")),
            dispatch_endpoint("ollama", "llama3", ModelCapabilities::for_model("llama3")),
        ]);
        let mut state = DispatchState::default();
        let route = route_request(&config, &mut state, &["openai"]).unwrap();
        assert_eq!(route.provider_id, "ollama");
    }

    #[test]
    fn dispatch_round_robin_rotates_providers() {
        let mut config = dispatch_test_config(vec![
            dispatch_endpoint("a", "m1", ModelCapabilities::default()),
            dispatch_endpoint("b", "m2", ModelCapabilities::default()),
        ]);
        config.load_balance = LoadBalanceStrategy::RoundRobin;
        let mut state = DispatchState::default();
        let first = route_request(&config, &mut state, &[]).unwrap().provider_id;
        let second = route_request(&config, &mut state, &[]).unwrap().provider_id;
        let third = route_request(&config, &mut state, &[]).unwrap().provider_id;
        assert_ne!(first, second);
        assert_eq!(first, third);
    }

    #[test]
    fn dispatch_least_loaded_prefers_lower_in_flight() {
        let mut config = dispatch_test_config(vec![
            {
                let mut p = dispatch_endpoint("busy", "m1", ModelCapabilities::default());
                p.in_flight_requests = 50;
                p
            },
            {
                let mut p = dispatch_endpoint("idle", "m2", ModelCapabilities::default());
                p.in_flight_requests = 1;
                p
            },
        ]);
        config.load_balance = LoadBalanceStrategy::LeastLoaded;
        let mut state = DispatchState::default();
        let route = route_request(&config, &mut state, &[]).unwrap();
        assert_eq!(route.provider_id, "idle");
    }

    #[test]
    fn dispatch_affinity_prefers_matching_group() {
        let mut config = dispatch_test_config(vec![
            {
                let mut p = dispatch_endpoint("a", "m1", ModelCapabilities::default());
                p.affinity_group = Some("tenant-1".into());
                p
            },
            {
                let mut p = dispatch_endpoint("b", "m2", ModelCapabilities::default());
                p.affinity_group = Some("tenant-2".into());
                p
            },
        ]);
        config.load_balance = LoadBalanceStrategy::Affinity;
        config.affinity_key = Some("tenant-2".into());
        let mut state = DispatchState::default();
        let route = route_request(&config, &mut state, &[]).unwrap();
        assert_eq!(route.provider_id, "b");
    }

    #[test]
    fn dispatch_affinity_session_overrides_config_key() {
        let mut config = dispatch_test_config(vec![
            {
                let mut p = dispatch_endpoint("a", "m1", ModelCapabilities::default());
                p.affinity_group = Some("tenant-1".into());
                p
            },
            {
                let mut p = dispatch_endpoint("b", "m2", ModelCapabilities::default());
                p.affinity_group = Some("tenant-2".into());
                p
            },
        ]);
        config.load_balance = LoadBalanceStrategy::Affinity;
        config.affinity_key = Some("tenant-2".into());
        let mut state = DispatchState {
            session_affinity: Some("tenant-1".into()),
            ..Default::default()
        };
        let route = route_request(&config, &mut state, &[]).unwrap();
        assert_eq!(route.provider_id, "a");
    }

    #[test]
    fn dispatch_select_fallback_follows_chain_order() {
        let mut config = dispatch_test_config(vec![
            dispatch_endpoint("openai", "gpt-4o", ModelCapabilities::for_model("gpt-4o")),
            dispatch_endpoint("ollama", "llama3", ModelCapabilities::for_model("llama3")),
        ]);
        config.fallback_chain = vec!["ollama".into(), "openai".into()];
        let plan = DispatchPlan {
            primary: RouteDecision {
                provider_id: "openai".into(),
                model: "gpt-4o".into(),
                reason: "primary".into(),
            },
            fallbacks: vec![],
        };
        let next = select_fallback(&config, &plan, "openai", &[])
            .unwrap()
            .unwrap();
        assert_eq!(next.provider_id, "ollama");
    }

    #[test]
    fn dispatch_select_fallback_skips_already_tried() {
        let config = dispatch_test_config(vec![
            dispatch_endpoint("openai", "gpt-4o", ModelCapabilities::for_model("gpt-4o")),
            dispatch_endpoint("ollama", "llama3", ModelCapabilities::for_model("llama3")),
        ]);
        let plan = DispatchPlan {
            primary: RouteDecision {
                provider_id: "openai".into(),
                model: "gpt-4o".into(),
                reason: "primary".into(),
            },
            fallbacks: vec![RouteDecision {
                provider_id: "ollama".into(),
                model: "llama3".into(),
                reason: "first fallback".into(),
            }],
        };
        assert!(
            select_fallback(&config, &plan, "ollama", &[])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn dispatch_select_fallback_all_failed_surfaces_errors() {
        let config = dispatch_test_config(vec![dispatch_endpoint(
            "only",
            "m",
            ModelCapabilities::default(),
        )]);
        let plan = DispatchPlan {
            primary: RouteDecision {
                provider_id: "only".into(),
                model: "m".into(),
                reason: "primary".into(),
            },
            fallbacks: vec![],
        };
        let err = select_fallback(
            &config,
            &plan,
            "only",
            &["timeout".into(), "rate limited".into()],
        )
        .unwrap_err();
        assert!(matches!(err, CoordinatorDispatchError::AllFailed(_)));
        if let CoordinatorDispatchError::AllFailed(errors) = err {
            assert_eq!(errors.len(), 2);
        }
    }

    #[test]
    fn dispatch_build_dispatch_plan_includes_fallbacks() {
        let mut config = dispatch_test_config(vec![
            dispatch_endpoint("openai", "gpt-4o", ModelCapabilities::for_model("gpt-4o")),
            dispatch_endpoint("ollama", "llama3", ModelCapabilities::for_model("llama3")),
        ]);
        config.fallback_chain = vec!["ollama".into()];
        let mut state = DispatchState::default();
        let plan = build_dispatch_plan(&config, &mut state).unwrap();
        assert!(!plan.fallbacks.is_empty());
        assert_eq!(plan.fallbacks[0].provider_id, "ollama");
    }

    #[test]
    fn dispatch_parse_coordinator_response_valid() {
        let json = r#"{
            "primary": {"provider_id":"openai","model":"gpt-4o","reason":"best"},
            "fallbacks": [{"provider_id":"ollama","model":"llama3","reason":"backup"}]
        }"#;
        let plan = parse_coordinator_response(json).unwrap();
        assert_eq!(plan.primary.provider_id, "openai");
        assert_eq!(plan.fallbacks.len(), 1);
    }

    #[test]
    fn dispatch_parse_coordinator_response_missing_primary() {
        let err = parse_coordinator_response(r#"{"fallbacks":[]}"#).unwrap_err();
        assert!(matches!(err, CoordinatorDispatchError::InvalidConfig(_)));
    }

    #[test]
    fn dispatch_parse_coordinator_response_invalid_json() {
        let err = parse_coordinator_response("not-json").unwrap_err();
        assert!(matches!(err, CoordinatorDispatchError::InvalidConfig(_)));
    }

    #[test]
    fn dispatch_parse_coordinator_response_invalid_fallbacks_type() {
        let err = parse_coordinator_response(
            r#"{"primary":{"provider_id":"a","model":"m","reason":"r"},"fallbacks":"nope"}"#,
        )
        .unwrap_err();
        assert!(matches!(err, CoordinatorDispatchError::InvalidConfig(_)));
    }

    #[test]
    fn dispatch_validate_config_empty_providers() {
        let config = CoordinatorConfig {
            providers: vec![],
            fallback_chain: vec![],
            load_balance: LoadBalanceStrategy::RoundRobin,
            requirements: CapabilityRequirements::default(),
            affinity_key: None,
        };
        let err = validate_coordinator_config(&config).unwrap_err();
        assert!(matches!(err, CoordinatorDispatchError::InvalidConfig(_)));
    }

    #[test]
    fn dispatch_validate_config_unknown_fallback() {
        let config = CoordinatorConfig {
            providers: vec![dispatch_endpoint(
                "openai",
                "gpt-4o",
                ModelCapabilities::default(),
            )],
            fallback_chain: vec!["missing".into()],
            load_balance: LoadBalanceStrategy::RoundRobin,
            requirements: CapabilityRequirements::default(),
            affinity_key: None,
        };
        let err = validate_coordinator_config(&config).unwrap_err();
        assert!(matches!(err, CoordinatorDispatchError::InvalidConfig(_)));
    }

    #[test]
    fn dispatch_validate_config_duplicate_ids() {
        let config = CoordinatorConfig {
            providers: vec![
                dispatch_endpoint("dup", "m1", ModelCapabilities::default()),
                dispatch_endpoint("dup", "m2", ModelCapabilities::default()),
            ],
            fallback_chain: vec![],
            load_balance: LoadBalanceStrategy::RoundRobin,
            requirements: CapabilityRequirements::default(),
            affinity_key: None,
        };
        let err = validate_coordinator_config(&config).unwrap_err();
        assert!(matches!(err, CoordinatorDispatchError::InvalidConfig(_)));
    }

    #[test]
    fn dispatch_coordinator_dispatch_error_display_no_available() {
        let err = CoordinatorDispatchError::NoAvailableProvider("none left".into());
        assert!(err.to_string().contains("no available provider"));
        assert!(err.to_string().contains("none left"));
    }

    #[test]
    fn dispatch_coordinator_dispatch_error_display_all_failed() {
        let err = CoordinatorDispatchError::AllFailed(vec!["e1".into(), "e2".into()]);
        let s = err.to_string();
        assert!(s.contains("all providers failed"));
        assert!(s.contains("e1"));
    }

    #[test]
    fn dispatch_coordinator_dispatch_error_display_invalid_config() {
        let err = CoordinatorDispatchError::InvalidConfig("bad chain".into());
        assert!(err.to_string().contains("invalid coordinator config"));
    }

    #[test]
    fn dispatch_route_decision_display() {
        let decision = RouteDecision {
            provider_id: "openai".into(),
            model: "gpt-4o".into(),
            reason: "best".into(),
        };
        let s = decision.to_string();
        assert!(s.contains("openai"));
        assert!(s.contains("gpt-4o"));
    }

    #[test]
    fn dispatch_plan_display() {
        let plan = DispatchPlan {
            primary: RouteDecision {
                provider_id: "openai".into(),
                model: "gpt-4o".into(),
                reason: "primary".into(),
            },
            fallbacks: vec![RouteDecision {
                provider_id: "ollama".into(),
                model: "llama3".into(),
                reason: "fb".into(),
            }],
        };
        let s = plan.to_string();
        assert!(s.contains("openai"));
        assert!(s.contains("ollama"));
    }

    #[test]
    fn dispatch_load_balance_strategy_display() {
        assert_eq!(LoadBalanceStrategy::RoundRobin.to_string(), "round_robin");
        assert_eq!(LoadBalanceStrategy::LeastLoaded.to_string(), "least_loaded");
        assert_eq!(LoadBalanceStrategy::Affinity.to_string(), "affinity");
    }

    #[test]
    fn dispatch_coordinator_config_clone() {
        let config = dispatch_test_config(vec![dispatch_endpoint(
            "openai",
            "gpt-4o",
            ModelCapabilities::default(),
        )]);
        let cloned = config.clone();
        assert_eq!(config.providers.len(), cloned.providers.len());
        assert_eq!(config.fallback_chain, cloned.fallback_chain);
    }

    #[test]
    fn dispatch_dispatch_state_clone_default() {
        let state = DispatchState::default();
        let cloned = state.clone();
        assert_eq!(state, cloned);
        assert_eq!(state.round_robin_cursor, 0);
    }

    #[test]
    fn dispatch_provider_endpoint_clone_debug() {
        let endpoint = dispatch_endpoint("openai", "gpt-4o", ModelCapabilities::default());
        let cloned = endpoint.clone();
        assert_eq!(endpoint, cloned);
        assert!(format!("{endpoint:?}").contains("openai"));
    }

    #[test]
    fn dispatch_route_decision_clone_eq() {
        let a = RouteDecision {
            provider_id: "a".into(),
            model: "m".into(),
            reason: "r".into(),
        };
        assert_eq!(a, a.clone());
    }

    #[test]
    fn dispatch_plan_clone_eq() {
        let plan = DispatchPlan {
            primary: RouteDecision {
                provider_id: "a".into(),
                model: "m".into(),
                reason: "r".into(),
            },
            fallbacks: vec![],
        };
        assert_eq!(plan, plan.clone());
    }

    #[test]
    fn dispatch_coordinator_dispatch_error_clone_eq() {
        let err = CoordinatorDispatchError::InvalidConfig("x".into());
        assert_eq!(err, err.clone());
    }

    #[test]
    fn dispatch_build_dispatch_plan_invalid_config_propagates() {
        let config = CoordinatorConfig {
            providers: vec![],
            fallback_chain: vec![],
            load_balance: LoadBalanceStrategy::RoundRobin,
            requirements: CapabilityRequirements::default(),
            affinity_key: None,
        };
        let err = build_dispatch_plan(&config, &mut DispatchState::default()).unwrap_err();
        assert!(matches!(err, CoordinatorDispatchError::InvalidConfig(_)));
    }

    #[test]
    fn dispatch_route_request_invalid_config_before_routing() {
        let config = CoordinatorConfig {
            providers: vec![],
            fallback_chain: vec![],
            load_balance: LoadBalanceStrategy::RoundRobin,
            requirements: CapabilityRequirements::default(),
            affinity_key: None,
        };
        let err = route_request(&config, &mut DispatchState::default(), &[]).unwrap_err();
        assert!(matches!(err, CoordinatorDispatchError::InvalidConfig(_)));
    }

    #[test]
    fn dispatch_select_fallback_respects_capability_on_chain() {
        let mut config = dispatch_test_config(vec![
            dispatch_endpoint(
                "no-tools",
                "basic",
                ModelCapabilities {
                    supports_tools: false,
                    ..Default::default()
                },
            ),
            dispatch_endpoint("openai", "gpt-4o", ModelCapabilities::for_model("gpt-4o")),
        ]);
        config.fallback_chain = vec!["no-tools".into(), "openai".into()];
        config.requirements = CapabilityRequirements::for_agent();
        let plan = DispatchPlan {
            primary: RouteDecision {
                provider_id: "openai".into(),
                model: "gpt-4o".into(),
                reason: "primary".into(),
            },
            fallbacks: vec![],
        };
        assert!(
            select_fallback(&config, &plan, "openai", &[])
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn test_message_to_role_content() {
        let msg = ConversationMessage::user("Hello");
        let (role, content) = msg.to_role_content();
        assert_eq!(role, "user");
        assert_eq!(content, "Hello");

        let msg = ConversationMessage::system("System prompt");
        let (role, content) = msg.to_role_content();
        assert_eq!(role, "system");
        assert_eq!(content, "System prompt");
    }
}
