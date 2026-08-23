//! Configurable Agent implementation
//!
//! This module provides a generic agent that can be configured via TOML.
//! It replaces the hardcoded agent implementations with a flexible,
//! configuration-driven approach.

#![allow(deprecated, reason = "deprecated AgentRegistry shims retained for one-release migration; internal use until loader cutover")]

use crate::{Agent, AgentResponse, ExecutionMetadata};
use crate::AgentConfig;
use ares_llm::coordinator::ConversationMessage;
use ares_llm::observability::{LlmCallRecord, ObservabilitySink, ToolCallRecord};
use ares_llm::{LLMClient, LLMResponse};
use ares_tools::Tools;
use ares_types::types::{AgentContext, AgentType, AppError, Result, ToolDefinition};
use async_trait::async_trait;
use std::future::Future;
use std::sync::Arc;
use cordis::{Context, CordisError, EventsService};

// cordis Phase6: runtime postgres availability via Service::check() — replaces compile-time #[cfg(feature="postgres")] branching
// Previously: `#[cfg(feature = "postgres")] token_budget_pool: Option<PgPool>`
// Now: always-present field guarded by `ctx.get::<PostgresService>().is_some()` / `PostgresService::check()`
// Handler example: `if ctx.get::<PostgresService>().is_some() { /* use token_budget_pool */ } else { /* fallback */ }`
// TODO: if ctx.get::<PostgresService>().is_some() { use db } else { fallback }
use cordis::Service;

/// Postgres availability as a Cordis Service — runtime check, not compile-time cfg.
///
/// `check()` returns `cfg!(feature = "postgres")` so callers can branch at runtime:
/// `if ctx.get::<PostgresService>().is_some_and(|s| s.check()) { /* postgres path */ }`
/// or `if ctx.get::<PostgresService>().is_some() { use db } else { fallback }`.
pub struct PostgresService;
impl Service for PostgresService {
    fn check(&self) -> bool {
        cfg!(feature = "postgres")
    }
}

struct ProviderLlm {
    provider_name: String,
    llm: Box<dyn LLMClient>,
}

struct LlmAttemptResponse {
    response: LLMResponse,
    provider_name: String,
    model_name: String,
}

/// A configurable agent that derives its behavior from TOML configuration
pub struct ConfigurableAgent {
    /// The agent's name/type identifier
    name: String,
    /// The agent type enum value
    agent_type: AgentType,
    /// The LLM client to use for generation
    llm: Box<dyn LLMClient>,
    /// The configured provider backing the LLM client
    provider_name: String,
    /// The system prompt from configuration
    system_prompt: String,
    /// Unified tools capability. Prefer `set_tools` + request ctx.
    tools: Option<Arc<Tools>>,
    /// Request Cordis context bound for this execution (Tools isolate + ExternalContext).
    cordis_ctx: Option<Arc<Context>>,
    /// Optional whitelist of tool names this agent is allowed to use.
    /// `None` means no tools are permitted.
    allowed_tools: Option<Vec<String>>,
    /// Maximum tool calling iterations
    max_tool_iterations: usize,
    /// Whether to execute tools in parallel
    parallel_tools: bool,
    /// Optional observability sink for run history logging
    observability: Option<Arc<dyn ObservabilitySink>>,
    /// Optional fallback LLM clients to try if primary fails
    fallback_llms: Vec<ProviderLlm>,
    /// Optional run id to associate with token usage records.
    run_id: Option<String>,
}

fn is_prebuilt_connector_tool(name: &str) -> bool {
    matches!(
        name,
        "google_calendar_list_events"
            | "google_calendar_create_event"
            | "google_calendar_delete_event"
            | "google_calendar_get_free_busy"
            | "gmail_send_email"
            | "gmail_list_messages"
            | "gmail_get_message"
            | "hubspot_get_contact"
            | "hubspot_create_contact"
            | "hubspot_list_deals"
            | "hubspot_create_deal"
            | "linkedin_create_share"
            | "linkedin_get_company_updates"
            | "salesforce_soql_query"
            | "salesforce_get_record"
            | "salesforce_create_record"
            | "slack_send_message"
            | "slack_list_channels"
            | "slack_upload_file"
    )
}

fn history_messages_from_payload(payload: &serde_json::Value) -> Vec<(String, String)> {
    payload
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    Some((
                        m.get("role")?.as_str()?.to_string(),
                        m.get("content")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn conversation_messages_from_payload(payload: &serde_json::Value) -> Vec<ConversationMessage> {
    payload
        .get("messages")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

fn tools_from_payload(payload: &serde_json::Value) -> Vec<ToolDefinition> {
    let Some(v) = payload.get("tools") else {
        return Vec::new();
    };
    if let Ok(defs) = serde_json::from_value::<Vec<ToolDefinition>>(v.clone()) {
        return defs;
    }
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let name = t.as_str().or_else(|| t.get("name")?.as_str())?;
                    Some(ToolDefinition {
                        name: name.to_string(),
                        description: String::new(),
                        parameters: serde_json::json!({}),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn attempt_to_generate_json(attempt: &LlmAttemptResponse) -> serde_json::Value {
    serde_json::json!({
        "content": attempt.response.content,
        "usage": attempt.response.usage,
        "model_name": attempt.model_name,
        "provider_name": attempt.provider_name,
        "tool_calls": attempt.response.tool_calls,
        "finish_reason": attempt.response.finish_reason,
    })
}

fn generate_denied(out: &serde_json::Value) -> bool {
    match out.get("deny") {
        Some(serde_json::Value::Bool(true)) => true,
        Some(serde_json::Value::String(s)) if !s.is_empty() => true,
        _ => false,
    }
}

/// Drive `waterfall_around` without capturing `Box<dyn LLMClient>` in the `'static`
/// core: the terminal core only shuttles the (possibly rewritten) payload back to
/// the caller, which runs generate on `&self.llm` and returns the JSON result.
async fn run_events_waterfall<F, Fut>(
    events: &EventsService,
    event: &str,
    payload: serde_json::Value,
    core: F,
) -> std::result::Result<serde_json::Value, CordisError>
where
    F: FnOnce(serde_json::Value) -> Fut,
    Fut: Future<Output = std::result::Result<serde_json::Value, CordisError>>,
{
    let (req_tx, req_rx) = tokio::sync::oneshot::channel();
    let (res_tx, res_rx) = tokio::sync::oneshot::channel();
    let wf = events.waterfall_around(event.to_string(), payload, move |p| async move {
        let _ = req_tx.send(p);
        match res_rx.await {
            Ok(r) => r,
            Err(_) => Err(CordisError::Fiber("llm generate core dropped".into())),
        }
    });
    tokio::pin!(wf);
    tokio::select! {
        wf_res = &mut wf => wf_res,
        req = req_rx => {
            match req {
                Ok(p) => {
                    let out = core(p).await;
                    let _ = res_tx.send(out);
                    wf.await
                }
                Err(_) => wf.await,
            }
        }
    }
}

impl ConfigurableAgent {
    /// Create a new configurable agent from TOML config
    ///
    /// Deprecated shim kept for one commit. New code should use
    /// `new_with_tool_service` with a service obtained via
    /// `ctx.get::<dyn ToolService>()`.
    #[deprecated(note = "use new_with_tool_service with ctx.get::<dyn ToolService>()")]
    pub fn new(
        name: &str,
        config: &AgentConfig,
        llm: Box<dyn LLMClient>,
        tools: Option<Arc<Tools>>,
    ) -> Self {
        Self::new_with_provider(name, config, llm, tools, config.model.clone())
    }

    /// Shared helper that resolves the common `agent_type`, `system_prompt`,
    /// and `allowed_tools` derived from `AgentConfig`. Extracted to eliminate
    /// the 93% near-duplicate bodies between `new_with_provider` and
    /// `new_with_provider_and_tool_service` reported by rust-doctor.
    fn resolve_common_fields(
        name: &str,
        config: &AgentConfig,
    ) -> (AgentType, String, Option<Vec<String>>) {
        let agent_type = Self::name_to_type(name);
        let system_prompt = config
            .system_prompt
            .clone()
            .unwrap_or_else(|| Self::default_system_prompt(name));
        // Use allowed_tools if present; otherwise fall back to legacy tools field.
        let allowed_tools = config.allowed_tools.clone().or_else(|| {
            if config.tools.is_empty() {
                None
            } else {
                Some(config.tools.clone())
            }
        });
        (agent_type, system_prompt, allowed_tools)
    }

    /// Create a new configurable agent with explicit provider metadata
    ///
    /// Deprecated shim. Prefer `new_with_provider_and_tool_service`.
    #[deprecated(note = "use new_with_provider_and_tool_service with ctx.get::<dyn ToolService>()")]
    pub fn new_with_provider(
        name: &str,
        config: &AgentConfig,
        llm: Box<dyn LLMClient>,
        tools: Option<Arc<Tools>>,
        provider_name: String,
    ) -> Self {
        let (agent_type, system_prompt, allowed_tools) =
            Self::resolve_common_fields(name, config);

        Self {
            name: name.to_string(),
            agent_type,
            llm,
            provider_name,
            system_prompt,
            tools,
            cordis_ctx: None,
            allowed_tools,
            max_tool_iterations: config.max_tool_iterations,
            parallel_tools: config.parallel_tools,
            observability: None,
            fallback_llms: Vec::new(),
            run_id: None,
        }
    }

    /// Create a new configurable agent with explicit parameters
    ///
    /// Deprecated shim. Prefer `with_tool_service_params`.
    #[deprecated(note = "use with_tool_service_params with ctx.get::<dyn ToolService>()")]
    #[allow(clippy::too_many_arguments)]
    pub fn with_params(
        name: &str,
        agent_type: AgentType,
        llm: Box<dyn LLMClient>,
        system_prompt: String,
        tools: Option<Arc<Tools>>,
        allowed_tools: Option<Vec<String>>,
        max_tool_iterations: usize,
        parallel_tools: bool,
    ) -> Self {
        Self {
            name: name.to_string(),
            agent_type,
            llm,
            provider_name: "unknown".to_string(),
            system_prompt,
            tools,
            cordis_ctx: None,
            allowed_tools,
            max_tool_iterations,
            parallel_tools,
            observability: None,
            fallback_llms: Vec::new(),
            run_id: None,
        }
    }

    // Preferred constructors that accept unified Tools from ctx.get::<Tools>().

    /// Create an agent wired to a unified ToolService.
    ///
    /// Obtain the service via `ctx.get::<dyn ToolService>()` and pass it here.
    /// This is the Cordis DI path. The service provides all tools with tenant
    /// precedence already handled.
    pub fn new_with_tool_service(
        name: &str,
        config: &AgentConfig,
        llm: Box<dyn LLMClient>,
        tool_service: Option<Arc<Tools>>,
    ) -> Self {
        Self::new_with_provider_and_tool_service(name, config, llm, tool_service, config.model.clone())
    }

    /// Create an agent with provider metadata and a unified ToolService.
    pub fn new_with_provider_and_tool_service(
        name: &str,
        config: &AgentConfig,
        llm: Box<dyn LLMClient>,
        tool_service: Option<Arc<Tools>>,
        provider_name: String,
    ) -> Self {
        let (agent_type, system_prompt, allowed_tools) =
            Self::resolve_common_fields(name, config);
        Self {
            name: name.to_string(),
            agent_type,
            llm,
            provider_name,
            system_prompt,
            tools: tool_service,
            cordis_ctx: None,
            allowed_tools,
            max_tool_iterations: config.max_tool_iterations,
            parallel_tools: config.parallel_tools,
            observability: None,
            fallback_llms: Vec::new(),
            run_id: None,
        }
    }

    /// Create an agent with explicit parameters and a unified ToolService.
    #[allow(clippy::too_many_arguments)]
    pub fn with_tool_service_params(
        name: &str,
        agent_type: AgentType,
        llm: Box<dyn LLMClient>,
        system_prompt: String,
        tool_service: Option<Arc<Tools>>,
        allowed_tools: Option<Vec<String>>,
        max_tool_iterations: usize,
        parallel_tools: bool,
    ) -> Self {
        Self {
            name: name.to_string(),
            agent_type,
            llm,
            provider_name: "unknown".to_string(),
            system_prompt,
            tools: tool_service,
            cordis_ctx: None,
            allowed_tools,
            max_tool_iterations,
            parallel_tools,
            observability: None,
            fallback_llms: Vec::new(),
            run_id: None,
        }
    }

    /// Create an agent by resolving the ToolService from a Context.
    ///
    /// This is the one line handlers should use: `ConfigurableAgent::new_from_context(&ctx, name, &config, llm)`.
    pub fn new_from_context(
        ctx: &Arc<Context>,
        name: &str,
        config: &AgentConfig,
        llm: Box<dyn LLMClient>,
    ) -> Self {
        let mut agent = Self::new_with_tool_service(name, config, llm, None);
        if let Some(tools) = ctx.get::<Tools>() {
            agent.set_tools(tools);
        }
        agent.bind_request_ctx(ctx.clone());
        agent
    }

    /// Same as `new_from_context` but with explicit provider name.
    pub fn new_from_context_with_provider(
        ctx: &Arc<Context>,
        name: &str,
        config: &AgentConfig,
        llm: Box<dyn LLMClient>,
        provider_name: String,
    ) -> Self {
        let mut agent = Self::new_with_provider_and_tool_service(name, config, llm, None, provider_name);
        if let Some(tools) = ctx.get::<Tools>() {
            agent.set_tools(tools);
        }
        agent.bind_request_ctx(ctx.clone());
        agent
    }

    /// Convert agent name to AgentType
    fn name_to_type(name: &str) -> AgentType {
        AgentType::from_string(name)
    }

    /// Get default system prompt for an agent type
    fn default_system_prompt(name: &str) -> String {
        match name.to_lowercase().as_str() {
            "router" => r#"You are a routing agent that classifies user queries.
Available agents: product, invoice, sales, finance, hr, orchestrator.
Respond with ONLY the agent name (one word, lowercase)."#
                .to_string(),

            "orchestrator" => r#"You are an orchestrator agent for complex queries.
Break down requests, delegate to specialists, and synthesize results."#
                .to_string(),

            "product" => r#"You are a Product Agent for product-related queries.
Handle catalog, specifications, inventory, and pricing questions."#
                .to_string(),

            "invoice" => r#"You are an Invoice Agent for billing queries.
Handle invoices, payments, and billing history."#
                .to_string(),

            "sales" => r#"You are a Sales Agent for sales analytics.
Handle performance metrics, revenue, and customer data."#
                .to_string(),

            "finance" => r#"You are a Finance Agent for financial analysis.
Handle statements, budgets, and expense management."#
                .to_string(),

            "hr" => r#"You are an HR Agent for human resources.
Handle employee info, policies, and benefits."#
                .to_string(),

            _ => format!("You are a {} agent.", name),
        }
    }

    /// Get the agent name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the max tool iterations setting
    pub fn max_tool_iterations(&self) -> usize {
        self.max_tool_iterations
    }

    /// Get the parallel tools setting
    pub fn parallel_tools(&self) -> bool {
        self.parallel_tools
    }

    /// Check if this agent may use tools (built-in or tenant-scoped via `Tools`).
    pub fn has_tools(&self) -> bool {
        self.tools.is_some()
    }

    /// Get the unified tools capability (if any).
    pub fn tools(&self) -> Option<&Arc<Tools>> {
        self.tools.as_ref()
    }

    /// Store the unified `Tools` capability used for list/resolve/dispatch.
    pub fn set_tools(&mut self, tools: Arc<Tools>) {
        self.tools = Some(tools);
    }

    /// Bind the request Cordis context so tool isolate labels and
    /// `ExternalContext` are visible during `execute`.
    pub fn bind_request_ctx(&mut self, ctx: Arc<Context>) {
        if let Some(tools) = ctx.get::<Tools>() {
            self.tools = Some(tools);
        }
        self.cordis_ctx = Some(ctx);
    }

    /// Get the list of allowed tool names for this agent.
    /// `None` means no tools are permitted.
    pub fn allowed_tools(&self) -> Option<&[String]> {
        self.allowed_tools.as_deref()
    }

    /// Override the allowed tools list at runtime (e.g. after merging with
    /// a per-tenant allowlist).
    pub fn set_allowed_tools(&mut self, allowed_tools: Option<Vec<String>>) {
        self.allowed_tools = allowed_tools;
    }

    /// Attach an observability sink to this agent.
    pub fn set_observability(&mut self, obs: Arc<dyn ObservabilitySink>) {
        self.observability = Some(obs);
    }

    /// Set fallback LLM clients to try if the primary fails.
    pub fn set_fallback_llms(&mut self, fallbacks: Vec<Box<dyn LLMClient>>) {
        self.fallback_llms = fallbacks
            .into_iter()
            .map(|llm| ProviderLlm {
                provider_name: "unknown".to_string(),
                llm,
            })
            .collect();
    }

    /// Set fallback LLM clients with provider names for observability and billing metadata.
    pub fn set_fallback_llms_with_providers(
        &mut self,
        fallbacks: Vec<(String, Box<dyn LLMClient>)>,
    ) {
        self.fallback_llms = fallbacks
            .into_iter()
            .map(|(provider_name, llm)| ProviderLlm { provider_name, llm })
            .collect();
    }

    /// Set the run id to associate with token usage records.
    pub fn set_run_id(&mut self, run_id: String) {
        self.run_id = Some(run_id);
    }

    async fn preflight_budget_check(&self, tenant_id: &str) -> Result<()> {
        let Some(ctx) = self.cordis_ctx.as_ref() else {
            return Ok(());
        };
        #[cfg(feature = "postgres")]
        if let Some(db) = ctx.get::<ares_store::TenantDb>() {
            let store = ares_store::token_budgets::TokenBudgetStore::new(db.pool());
            let status = store.check_budget(tenant_id).await?;
            if status.would_exceed {
                return Err(AppError::RateLimited(format!(
                    "Tenant {} token budget exceeded ({} / {})",
                    tenant_id, status.tokens_used, status.token_limit
                )));
            }
        }
        Ok(())
    }

    async fn record_and_check_budget(
        &self,
        tenant_id: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
    ) -> Result<()> {
        let Some(ctx) = self.cordis_ctx.as_ref() else {
            return Ok(());
        };
        #[cfg(feature = "postgres")]
        if let Some(db) = ctx.get::<ares_store::TenantDb>() {
            let store = ares_store::token_budgets::TokenBudgetStore::new(db.pool());
            store
                .record_usage(
                    tenant_id,
                    self.run_id.as_deref(),
                    &self.name,
                    self.llm.model_name(),
                    prompt_tokens,
                    completion_tokens,
                )
                .await?;
            let status = store.check_budget(tenant_id).await?;
            if status.percentage >= status.alert_threshold {
                tracing::warn!(
                    tenant_id,
                    usage_pct = status.percentage,
                    threshold = status.alert_threshold,
                    "Token budget alert threshold crossed"
                );
            }
            if status.would_exceed {
                tracing::warn!(
                    tenant_id,
                    remaining = status.remaining,
                    "Tenant token budget would be exceeded"
                );
            }
        }
        Ok(())
    }

    /// Try the primary LLM, then each fallback in order.
    async fn try_generate_with_history(
        &self,
        messages: &[(String, String)],
    ) -> Result<LlmAttemptResponse> {
        let ctx = self.cordis_ctx.clone().unwrap_or_else(Context::new_root);
        let Some(events) = ctx.get::<EventsService>() else {
            return self.generate_with_history_direct(messages).await;
        };
        let orig: Vec<(String, String)> = messages.to_vec();
        let payload = serde_json::json!({
            "messages": orig.iter().map(|(role, content)| {
                serde_json::json!({ "role": role, "content": content })
            }).collect::<Vec<_>>(),
        });
        let out = run_events_waterfall(&events, "llm.generate", payload, |payload| async move {
            let parsed = history_messages_from_payload(&payload);
            let msgs = if parsed.is_empty() { orig } else { parsed };
            match self.generate_with_history_direct(&msgs).await {
                Ok(attempt) => Ok(attempt_to_generate_json(&attempt)),
                Err(e) => Err(CordisError::Fiber(e.to_string())),
            }
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
        self.generate_attempt_from_payload(out, "llm.generate")
    }

    async fn generate_with_history_direct(
        &self,
        messages: &[(String, String)],
    ) -> Result<LlmAttemptResponse> {
        match self.llm.generate_with_history(messages).await {
            Ok(response) => Ok(LlmAttemptResponse {
                response,
                provider_name: self.provider_name.clone(),
                model_name: self.llm.model_name().to_string(),
            }),
            Err(e) => {
                let primary_error = e.to_string();
                let mut fallback_errors = Vec::new();
                for (i, fallback) in self.fallback_llms.iter().enumerate() {
                    tracing::warn!(
                        agent = %self.name,
                        fallback_idx = %i,
                        "Primary LLM failed, trying fallback"
                    );
                    match fallback.llm.generate_with_history(messages).await {
                        Ok(response) => {
                            tracing::info!(
                                agent = %self.name,
                                fallback_idx = %i,
                                provider = %fallback.provider_name,
                                "Fallback LLM succeeded"
                            );
                            return Ok(LlmAttemptResponse {
                                response,
                                provider_name: fallback.provider_name.clone(),
                                model_name: fallback.llm.model_name().to_string(),
                            });
                        }
                        Err(fallback_error) => {
                            fallback_errors.push(format!("fallback[{i}]: {fallback_error}"));
                        }
                    }
                }
                if fallback_errors.is_empty() {
                    Err(e)
                } else {
                    Err(AppError::LLM(format!(
                        "All LLM providers failed for agent '{}'; primary: {}; {}",
                        self.name,
                        primary_error,
                        fallback_errors.join("; ")
                    )))
                }
            }
        }
    }

    /// Try the primary LLM with tools, then each fallback in order.
    async fn try_generate_with_tools_and_history(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmAttemptResponse> {
        let ctx = self.cordis_ctx.clone().unwrap_or_else(Context::new_root);
        let Some(events) = ctx.get::<EventsService>() else {
            return self.generate_with_tools_and_history_direct(messages, tools).await;
        };
        let orig_messages = messages.to_vec();
        let orig_tools = tools.to_vec();
        let payload = serde_json::json!({
            "messages": orig_messages,
            "tools": orig_tools,
        });
        let out = run_events_waterfall(&events, "llm.generate_tools", payload, |payload| async move {
            let parsed_msgs = conversation_messages_from_payload(&payload);
            let msgs = if parsed_msgs.is_empty() {
                orig_messages
            } else {
                parsed_msgs
            };
            let parsed_tools = tools_from_payload(&payload);
            let tool_defs = if parsed_tools.is_empty() && payload.get("tools").is_none() {
                orig_tools
            } else {
                parsed_tools
            };
            match self
                .generate_with_tools_and_history_direct(&msgs, &tool_defs)
                .await
            {
                Ok(attempt) => Ok(attempt_to_generate_json(&attempt)),
                Err(e) => Err(CordisError::Fiber(e.to_string())),
            }
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
        self.generate_attempt_from_payload(out, "llm.generate_tools")
    }

    async fn generate_with_tools_and_history_direct(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmAttemptResponse> {
        match self
            .llm
            .generate_with_tools_and_history(messages, tools)
            .await
        {
            Ok(response) => Ok(LlmAttemptResponse {
                response,
                provider_name: self.provider_name.clone(),
                model_name: self.llm.model_name().to_string(),
            }),
            Err(e) => {
                let primary_error = e.to_string();
                let mut fallback_errors = Vec::new();
                for (i, fallback) in self.fallback_llms.iter().enumerate() {
                    tracing::warn!(
                        agent = %self.name,
                        fallback_idx = %i,
                        "Primary LLM (tools) failed, trying fallback"
                    );
                    match fallback
                        .llm
                        .generate_with_tools_and_history(messages, tools)
                        .await
                    {
                        Ok(response) => {
                            tracing::info!(
                                agent = %self.name,
                                fallback_idx = %i,
                                provider = %fallback.provider_name,
                                "Fallback LLM (tools) succeeded"
                            );
                            return Ok(LlmAttemptResponse {
                                response,
                                provider_name: fallback.provider_name.clone(),
                                model_name: fallback.llm.model_name().to_string(),
                            });
                        }
                        Err(fallback_error) => {
                            fallback_errors.push(format!("fallback[{i}]: {fallback_error}"));
                        }
                    }
                }
                if fallback_errors.is_empty() {
                    Err(e)
                } else {
                    Err(AppError::LLM(format!(
                        "All LLM providers failed for agent '{}'; primary: {}; {}",
                        self.name,
                        primary_error,
                        fallback_errors.join("; ")
                    )))
                }
            }
        }
    }

    fn generate_attempt_from_payload(
        &self,
        out: serde_json::Value,
        event: &str,
    ) -> Result<LlmAttemptResponse> {
        if generate_denied(&out) {
            let reason = out
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("denied");
            return Err(AppError::InvalidInput(format!("{event} {reason}")));
        }
        let content = out
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let usage = out
            .get("usage")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok());
        let model_name = out
            .get("model_name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| self.llm.model_name().to_string());
        let provider_name = out
            .get("provider_name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| self.provider_name.clone());
        let tool_calls = out
            .get("tool_calls")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let finish_reason = out
            .get("finish_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("stop")
            .to_string();
        Ok(LlmAttemptResponse {
            response: LLMResponse {
                content,
                tool_calls,
                finish_reason,
                usage,
            },
            provider_name,
            model_name,
        })
    }

    /// Get tool definitions for this agent.
    ///
    /// If `allowed_tools` is set, returns only those tools (if enabled).
    /// Otherwise returns no tools: tool use is deny-by-default.
    pub fn get_filtered_tool_definitions(&self) -> Vec<ToolDefinition> {
        let (Some(tools), Some(allowed)) = (&self.tools, &self.allowed_tools) else {
            return Vec::new();
        };
        if allowed.is_empty() {
            return Vec::new();
        }
        let ctx = self.cordis_ctx.clone().unwrap_or_else(Context::new_root);
        tools
            .list(&ctx)
            .into_iter()
            .filter(|def| allowed.iter().any(|a| a == &def.name))
            .collect()
    }

    /// Check if a specific tool is allowed for this agent.
    /// When no whitelist is set, no tool is allowed.
    pub fn can_use_tool(&self, tool_name: &str) -> bool {
        let whitelisted = match &self.allowed_tools {
            Some(allowed) => allowed.iter().any(|allowed| allowed == tool_name),
            None => false,
        };
        if !whitelisted {
            return false;
        }
        // Presence of Tools matches the old empty-registry default-enabled path.
        self.tools.is_some()
    }

    fn tenant_scoped_builtin_args(
        &self,
        name: &str,
        mut args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        if !is_prebuilt_connector_tool(name) {
            return Ok(args);
        }
        let Some(tenant_id) = self.connector_tenant_id() else {
            return Err(AppError::InvalidInput(
                "tenant_id is required for connector tool execution".to_string(),
            ));
        };
        let serde_json::Value::Object(map) = &mut args else {
            return Err(AppError::InvalidInput(
                "connector tool arguments must be an object".to_string(),
            ));
        };
        if let Some(provided) = map.get("tenant_id").and_then(|value| value.as_str()) {
            if provided != tenant_id {
                return Err(AppError::Auth(
                    "connector tenant_id does not match executing tenant".to_string(),
                ));
            }
        }
        map.insert(
            "tenant_id".to_string(),
            serde_json::Value::String(tenant_id),
        );
        Ok(args)
    }

    fn connector_tenant_id(&self) -> Option<String> {
        let ctx = self.cordis_ctx.as_ref()?;
        let id = crate::user_id_from_ctx(ctx, "");
        if id.is_empty() {
            None
        } else {
            Some(id)
        }
    }

    /// Execute a single tool call via `Tools::resolve(ctx, name)`.
    async fn dispatch_tool(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let Some(tools) = &self.tools else {
            return Err(AppError::NotFound(format!("Tool not found: {name}")));
        };
        let ctx = self.cordis_ctx.clone().unwrap_or_else(Context::new_root);
        let args = self.tenant_scoped_builtin_args(name, args)?;
        tools.execute(&ctx, name, args).await
    }

    fn observed_tool_type(&self, name: &str, is_builtin: bool) -> String {
        if is_builtin {
            return "builtin".to_string();
        }
        let _ = name;
        "runtime".to_string()
    }

    fn effective_system_prompt(&self) -> String {
        if let Some(ctx) = &self.cordis_ctx {
            if let Some(ext) = ctx.get::<crate::external_context::ExternalContext>() {
                if !ext.0.is_empty() {
                    tracing::debug!(
                        agent = %self.name,
                        ctx_len = ext.0.len(),
                        "External context injected into system prompt"
                    );
                    return format!(
                        "{}

{}

When referencing facts above, cite [E1], [E2] etc.",
                        ext.0, self.system_prompt
                    );
                }
            }
        }
        self.system_prompt.clone()
    }

    /// Execute the agent with tool-calling support (multi-turn loop).
    async fn execute_with_tools(
        &self,
        input: &str,
        context: &AgentContext,
    ) -> Result<AgentResponse> {
        use ares_llm::client::TokenUsage;

        let tools = self.get_filtered_tool_definitions();
        tracing::debug!(
            agent = %self.name,
            allowed_tools = ?self.allowed_tools,
            tool_count = tools.len(),
            "execute_with_tools: tool definitions loaded"
        );

        let mut messages: Vec<ConversationMessage> = Vec::new();

        // Inject external context if a ContextProvider is configured
        // OSS: NoOpContextProvider returns None. Managed: ErukaContextProvider returns knowledge states.
        let effective_prompt = self.effective_system_prompt();
        messages.push(ConversationMessage::system(&effective_prompt));

        // Add recent conversation history (last 5 messages)
        for msg in context.conversation_history.iter().rev().take(5).rev() {
            let cm = match msg.role {
                ares_types::types::MessageRole::User => ConversationMessage::user(&msg.content),
                ares_types::types::MessageRole::Assistant => {
                    ConversationMessage::assistant(&msg.content, vec![])
                }
                _ => ConversationMessage::system(&msg.content),
            };
            messages.push(cm);
        }

        messages.push(ConversationMessage::user(input));

        let mut total_usage = TokenUsage::default();
        let mut last_provider_name = self.provider_name.clone();
        let mut last_model_name = self.llm.model_name().to_string();

        for iteration in 0..self.max_tool_iterations {
            self.preflight_budget_check(&context.user_id).await?;

            let llm_start = std::time::Instant::now();
            let attempt = self
                .try_generate_with_tools_and_history(&messages, &tools)
                .await?;
            let llm_latency = llm_start.elapsed().as_millis() as i64;
            last_provider_name = attempt.provider_name;
            last_model_name = attempt.model_name;
            let response = attempt.response;

            {
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
                self.record_and_check_budget(&context.user_id, prompt_tok, completion_tok)
                    .await?;
            }

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
                let record = LlmCallRecord {
                    step_index: iteration as i32,
                    provider: last_provider_name.clone(),
                    model: last_model_name.clone(),
                    prompt_tokens: prompt_tok,
                    completion_tokens: completion_tok,
                    latency_ms: llm_latency,
                    status: "success".to_string(),
                };
                let _ = obs.log_llm_call(record).await;
            }

            if let Some(usage) = &response.usage {
                total_usage = TokenUsage::new(
                    total_usage.prompt_tokens + usage.prompt_tokens,
                    total_usage.completion_tokens + usage.completion_tokens,
                );
            }

            if response.tool_calls.is_empty() {
                return Ok(AgentResponse {
                    content: response.content,
                    usage: Some(total_usage),
                    metadata: Some(ExecutionMetadata {
                        model_name: last_model_name,
                        provider_name: last_provider_name,
                    }),
                });
            }

            // Add assistant message with tool calls
            messages.push(ConversationMessage::assistant(
                &response.content,
                response.tool_calls.clone(),
            ));

            // Execute each tool call and add results
            for tc in &response.tool_calls {
                // Runtime enforcement of allowed_tools (DIR1-46): deny-by-default.
                if !self.can_use_tool(&tc.name) {
                    tracing::warn!(
                        agent = %self.name,
                        tool = %tc.name,
                        allowed_tools = ?self.allowed_tools,
                        "Tool not in allowed_tools list — denying execution"
                    );
                    return Err(AppError::Auth(format!(
                        "Tool '{}' is not allowed for this agent",
                        tc.name
                    )));
                }

                let tool_start = std::time::Instant::now();
                let is_builtin = {
                    let ctx = self.cordis_ctx.clone().unwrap_or_else(Context::new_root);
                    self.tools
                        .as_ref()
                        .and_then(|t| t.resolve(&ctx, &tc.name))
                        .is_some()
                };
                let tool_type = self.observed_tool_type(&tc.name, is_builtin);
                let result = self.dispatch_tool(&tc.name, tc.arguments.clone()).await;
                let tool_latency = tool_start.elapsed().as_millis() as i64;
                let result_value = match result {
                    Ok(v) => v,
                    Err(e) => serde_json::json!({"error": e.to_string()}),
                };

                // Log the tool call
                if let Some(obs) = &self.observability {
                    let status = if result_value.get("error").is_some() {
                        "error".to_string()
                    } else {
                        "success".to_string()
                    };
                    let tool_record = ToolCallRecord {
                        step_index: iteration as i32,
                        tool_name: tc.name.clone(),
                        tool_type,
                        arguments: tc.arguments.clone(),
                        result: Some(result_value.clone()),
                        latency_ms: tool_latency,
                        status,
                    };
                    let _ = obs.log_tool_call(tool_record).await;
                }

                messages.push(ConversationMessage::tool_result(&tc.id, &result_value));
            }
        }

        // Max iterations reached — make ONE final LLM call without tools to get synthesis
        // Bug #7 fix: the last assistant message has empty content (it was a tool-call message).
        // We need the LLM to synthesize a final response from all the tool results.
        tracing::warn!(
            agent = %self.name,
            "Max tool iterations ({}) reached — making final synthesis call",
            self.max_tool_iterations
        );
        self.preflight_budget_check(&context.user_id).await?;

        let synth_start = std::time::Instant::now();
        let final_response = self
            .try_generate_with_tools_and_history(&messages, &[])
            .await;
        let synth_latency = synth_start.elapsed().as_millis() as i64;
        if let Ok(attempt) = &final_response {
            last_provider_name = attempt.provider_name.clone();
            last_model_name = attempt.model_name.clone();
        }

        if let Ok(attempt) = &final_response {
            let prompt_tok = attempt
                .response
                .usage
                .as_ref()
                .map(|u| u.prompt_tokens as i64)
                .unwrap_or(0);
            let completion_tok = attempt
                .response
                .usage
                .as_ref()
                .map(|u| u.completion_tokens as i64)
                .unwrap_or(0);
            let _ = self
                .record_and_check_budget(&context.user_id, prompt_tok, completion_tok)
                .await;
        }

        // Log the final synthesis call
        if let Some(obs) = &self.observability {
            let (prompt_tok, completion_tok, status) = match &final_response {
                Ok(attempt) => (
                    attempt
                        .response
                        .usage
                        .as_ref()
                        .map(|u| u.prompt_tokens as i64)
                        .unwrap_or(0),
                    attempt
                        .response
                        .usage
                        .as_ref()
                        .map(|u| u.completion_tokens as i64)
                        .unwrap_or(0),
                    "success".to_string(),
                ),
                Err(_) => (0, 0, "error".to_string()),
            };
            let record = LlmCallRecord {
                step_index: self.max_tool_iterations as i32,
                provider: last_provider_name.clone(),
                model: last_model_name.clone(),
                prompt_tokens: prompt_tok,
                completion_tokens: completion_tok,
                latency_ms: synth_latency,
                status,
            };
            let _ = obs.log_llm_call(record).await;
        }

        let content = match final_response {
            Ok(attempt) if !attempt.response.content.is_empty() => attempt.response.content,
            Ok(_) => {
                // Final call also returned empty — find any non-empty assistant content
                messages
                    .iter()
                    .rev()
                    .find(|m| {
                        m.role == ares_llm::coordinator::MessageRole::Assistant
                            && !m.content.is_empty()
                    })
                    .map(|m| m.content.clone())
                    .unwrap_or_else(|| {
                        "Agent completed tool calls but could not generate a final response."
                            .to_string()
                    })
            }
            Err(e) => {
                tracing::error!(error = %e, "Final synthesis call failed");
                // Still try to return something useful
                messages
                    .iter()
                    .rev()
                    .find(|m| {
                        m.role == ares_llm::coordinator::MessageRole::Assistant
                            && !m.content.is_empty()
                    })
                    .map(|m| m.content.clone())
                    .unwrap_or_else(|| format!("Agent completed but synthesis failed: {}", e))
            }
        };

        Ok(AgentResponse {
            content,
            usage: Some(total_usage),
            metadata: Some(ExecutionMetadata {
                model_name: last_model_name,
                provider_name: last_provider_name,
            }),
        })
    }
}

#[async_trait]
impl Agent for ConfigurableAgent {
    async fn execute(&self, input: &str, context: &AgentContext) -> Result<AgentResponse> {
        if self.has_tools() {
            tracing::debug!(agent = %self.name, "execute: using tool-calling path");
            return self.execute_with_tools(input, context).await;
        }
        tracing::debug!(agent = %self.name, "execute: no tools, using simple path");

        // Build context with conversation history if available
        // Inject external context if a ContextProvider is configured
        let effective_prompt = self.effective_system_prompt();
        let mut messages = vec![("system".to_string(), effective_prompt)];

        // Add user memory if available
        if let Some(memory) = &context.user_memory {
            let memory_context = format!(
                "User preferences: {}",
                memory
                    .preferences
                    .iter()
                    .map(|p| format!("{}: {}", p.key, p.value))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            messages.push(("system".to_string(), memory_context));
        }

        // Add recent conversation history (last 5 messages)
        for msg in context.conversation_history.iter().rev().take(5).rev() {
            let role = match msg.role {
                ares_types::types::MessageRole::User => "user",
                ares_types::types::MessageRole::Assistant => "assistant",
                _ => "system",
            };
            messages.push((role.to_string(), msg.content.clone()));
        }

        messages.push(("user".to_string(), input.to_string()));

        self.preflight_budget_check(&context.user_id).await?;

        let llm_start = std::time::Instant::now();
        let attempt = self.try_generate_with_history(&messages).await?;
        let llm_latency = llm_start.elapsed().as_millis() as i64;
        let provider_name = attempt.provider_name;
        let model_name = attempt.model_name;
        let llm_response = attempt.response;

        {
            let prompt_tok = llm_response
                .usage
                .as_ref()
                .map(|u| u.prompt_tokens as i64)
                .unwrap_or(0);
            let completion_tok = llm_response
                .usage
                .as_ref()
                .map(|u| u.completion_tokens as i64)
                .unwrap_or(0);
            self.record_and_check_budget(&context.user_id, prompt_tok, completion_tok)
                .await?;
        }

        // Log the LLM call
        if let Some(obs) = &self.observability {
            let prompt_tok = llm_response
                .usage
                .as_ref()
                .map(|u| u.prompt_tokens as i64)
                .unwrap_or(0);
            let completion_tok = llm_response
                .usage
                .as_ref()
                .map(|u| u.completion_tokens as i64)
                .unwrap_or(0);
            let record = LlmCallRecord {
                step_index: 0,
                provider: provider_name.clone(),
                model: model_name.clone(),
                prompt_tokens: prompt_tok,
                completion_tokens: completion_tok,
                latency_ms: llm_latency,
                status: "success".to_string(),
            };
            let _ = obs.log_llm_call(record).await;
        }

        Ok(AgentResponse {
            content: llm_response.content,
            usage: llm_response.usage,
            metadata: Some(ExecutionMetadata {
                model_name,
                provider_name,
            }),
        })
    }

    fn system_prompt(&self) -> String {
        self.system_prompt.clone()
    }

    fn agent_type(&self) -> AgentType {
        self.agent_type.clone()
    }
}

/// Convert a resolved [`UserAgent`] row into an [`AgentConfig`] for agent creation.
///
/// Used by `Execute` and handlers to bridge DB resolution → agent instantiation.
#[cfg(feature = "postgres")]
pub fn agent_config_from_user_agent(user_agent: &ares_store::postgres::UserAgent) -> AgentConfig {
    AgentConfig {
        model: user_agent.model.clone(),
        system_prompt: user_agent.system_prompt.clone(),
        tools: user_agent.tools_vec(),
        max_tool_iterations: user_agent.max_tool_iterations as usize,
        parallel_tools: user_agent.parallel_tools,
        allowed_tools: None,
        extra: std::collections::HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_tools::Tool;
    use crate::AgentConfig;
    use ares_llm::client::TokenUsage;
    use ares_llm::LLMResponse;
    use ares_types::types::{Message, MessageRole, Preference, ToolCall, UserMemory};
    use chrono::Utc;
    use std::collections::{HashMap, VecDeque};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    // ============== Shared MockLLM ==============

    /// Configurable mock LLM client shared by all tests.
    ///
    /// - `content` is returned for `generate_with_history` and simple methods.
    /// - `tool_responses` queue feeds `generate_with_tools_and_history` —
    ///   popped front-to-back on each call; falls back to default when empty.
    struct MockLLM {
        content: String,
        tool_responses: Arc<Mutex<VecDeque<LLMResponse>>>,
        generated: Arc<AtomicBool>,
        echo_last: bool,
    }

    impl MockLLM {
        fn new() -> Self {
            Self::with_content("mock")
        }

        fn with_content(content: &str) -> Self {
            Self {
                content: content.to_string(),
                tool_responses: Arc::new(Mutex::new(VecDeque::new())),
                generated: Arc::new(AtomicBool::new(false)),
                echo_last: false,
            }
        }

        /// Supply a sequence of responses for `generate_with_tools_and_history`.
        /// Each call pops the front; when the queue is exhausted the default is used.
        fn with_tool_responses(responses: Vec<LLMResponse>) -> Self {
            Self {
                content: "mock".to_string(),
                tool_responses: Arc::new(Mutex::new(responses.into())),
                generated: Arc::new(AtomicBool::new(false)),
                echo_last: false,
            }
        }

        fn echo_last() -> Self {
            let mut llm = Self::new();
            llm.echo_last = true;
            llm
        }

        fn with_generated_flag() -> (Self, Arc<AtomicBool>) {
            let generated = Arc::new(AtomicBool::new(false));
            (
                Self {
                    content: "should-not-appear".to_string(),
                    tool_responses: Arc::new(Mutex::new(VecDeque::new())),
                    generated: Arc::clone(&generated),
                    echo_last: false,
                },
                generated,
            )
        }
    }

    #[async_trait]
    impl LLMClient for MockLLM {
        async fn generate(&self, _: &str) -> Result<String> {
            Ok(self.content.clone())
        }
        async fn generate_with_system(&self, _: &str, _: &str) -> Result<String> {
            Ok(self.content.clone())
        }
        async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<LLMResponse> {
            self.generated.store(true, Ordering::SeqCst);
            let content = if self.echo_last {
                messages
                    .last()
                    .map(|(_, c)| c.clone())
                    .unwrap_or_else(|| self.content.clone())
            } else {
                self.content.clone()
            };
            Ok(LLMResponse {
                content,
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
                usage: None,
            })
        }
        async fn generate_with_tools(&self, _: &str, _: &[ToolDefinition]) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: self.content.clone(),
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
                usage: None,
            })
        }
        async fn generate_with_tools_and_history(
            &self,
            _: &[ares_llm::coordinator::ConversationMessage],
            _: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            let mut q = self.tool_responses.lock().unwrap();
            Ok(q.pop_front().unwrap_or(LLMResponse {
                content: self.content.clone(),
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
                usage: None,
            }))
        }
        async fn stream(
            &self,
            _: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }
        async fn stream_with_system(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }
        async fn stream_with_history(
            &self,
            _: &[(String, String)],
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }
        fn model_name(&self) -> &str {
            "mock"
        }
    }

    // ============== Shared MockTool ==============

    struct MockTool {
        name: String,
        description: String,
    }

    struct EchoArgsTool {
        name: String,
    }

    impl MockTool {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                description: format!("Mock tool: {}", name),
            }
        }
    }

    #[async_trait]
    impl ares_tools::Tool for EchoArgsTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "Echoes arguments"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type":"object"})
        }
        async fn execute(&self, args: serde_json::Value) -> Result<serde_json::Value> {
            Ok(args)
        }
    }

    #[async_trait]
    impl ares_tools::Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            &self.description
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value) -> Result<serde_json::Value> {
            Ok(serde_json::json!({"result": "ok"}))
        }
    }

    // ============== Helpers ==============

    fn make_config(tools: Vec<&str>, system_prompt: Option<&str>) -> AgentConfig {
        AgentConfig {
            model: "default".to_string(),
            system_prompt: system_prompt.map(String::from),
            tools: tools.into_iter().map(String::from).collect(),
            allowed_tools: None,
            max_tool_iterations: 5,
            parallel_tools: false,
            extra: HashMap::new(),
        }
    }

    fn make_context() -> AgentContext {
        AgentContext {
            user_id: "test-user".to_string(),
            session_id: "test-session".to_string(),
            conversation_history: vec![],
            user_memory: None,
        }
    }

    fn make_context_with_history(history: Vec<(MessageRole, &str)>) -> AgentContext {
        AgentContext {
            user_id: "test-user".to_string(),
            session_id: "test-session".to_string(),
            conversation_history: history
                .into_iter()
                .map(|(role, content)| Message {
                    role,
                    content: content.to_string(),
                    timestamp: Utc::now(),
                })
                .collect(),
            user_memory: None,
        }
    }

    fn make_tool_response(content: &str, calls: Vec<ToolCall>) -> LLMResponse {
        let finish_reason = if calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        };
        LLMResponse {
            content: content.to_string(),
            tool_calls: calls,
            finish_reason: finish_reason.to_string(),
            usage: Some(TokenUsage::new(10, 5)),
        }
    }

    fn make_tools_with_tool(name: &str) -> Arc<Tools> {
        Arc::new(Tools::from_static([
            Arc::new(MockTool::new(name)) as Arc<dyn Tool>,
        ]))
    }

    fn make_tools_with_echo_tool(name: &str) -> Arc<Tools> {
        Arc::new(Tools::from_static([
            Arc::new(EchoArgsTool {
                name: name.to_string(),
            }) as Arc<dyn Tool>,
        ]))
    }

    // ==========================================================
    //  2. default_system_prompt — all variants
    // ==========================================================

    #[test]
    fn test_default_system_prompt_router() {
        let p = ConfigurableAgent::default_system_prompt("router");
        assert!(
            p.contains("routing"),
            "router prompt should mention 'routing'"
        );
    }

    #[test]
    fn test_default_system_prompt_orchestrator() {
        let p = ConfigurableAgent::default_system_prompt("orchestrator");
        assert!(p.contains("orchestrator"));
    }

    #[test]
    fn test_default_system_prompt_product() {
        let p = ConfigurableAgent::default_system_prompt("product");
        assert!(p.contains("Product"));
    }

    #[test]
    fn test_default_system_prompt_invoice() {
        let p = ConfigurableAgent::default_system_prompt("invoice");
        assert!(p.contains("Invoice"));
    }

    #[test]
    fn test_default_system_prompt_sales() {
        let p = ConfigurableAgent::default_system_prompt("sales");
        assert!(p.contains("Sales"));
    }

    #[test]
    fn test_default_system_prompt_finance() {
        let p = ConfigurableAgent::default_system_prompt("finance");
        assert!(p.contains("Finance"));
    }

    #[test]
    fn test_default_system_prompt_hr() {
        let p = ConfigurableAgent::default_system_prompt("hr");
        assert!(p.contains("HR"));
    }

    #[test]
    fn test_default_system_prompt_unknown_name() {
        let p = ConfigurableAgent::default_system_prompt("unknown_name");
        assert_eq!(p, "You are a unknown_name agent.");
    }

    #[test]
    fn test_default_system_prompt_case_insensitive() {
        let p = ConfigurableAgent::default_system_prompt("ROUTER");
        assert!(p.contains("routing"), "ROUTER should match router branch");
    }

    // ==========================================================
    //  3. name_to_type — more edge cases
    // ==========================================================

    #[test]
    fn test_name_to_type_router() {
        assert!(matches!(
            ConfigurableAgent::name_to_type("router"),
            AgentType::Router
        ));
    }

    #[test]
    fn test_name_to_type_invoice() {
        assert!(matches!(
            ConfigurableAgent::name_to_type("invoice"),
            AgentType::Invoice
        ));
    }

    #[test]
    fn test_name_to_type_sales() {
        assert!(matches!(
            ConfigurableAgent::name_to_type("sales"),
            AgentType::Sales
        ));
    }

    #[test]
    fn test_name_to_type_finance() {
        assert!(matches!(
            ConfigurableAgent::name_to_type("finance"),
            AgentType::Finance
        ));
    }

    #[test]
    fn test_name_to_type_hr() {
        assert!(matches!(
            ConfigurableAgent::name_to_type("hr"),
            AgentType::HR
        ));
    }

    #[test]
    fn test_name_to_type_orchestrator() {
        assert!(matches!(
            ConfigurableAgent::name_to_type("orchestrator"),
            AgentType::Orchestrator
        ));
    }

    #[test]
    fn test_name_to_type_product_upper() {
        assert!(matches!(
            ConfigurableAgent::name_to_type("PRODUCT"),
            AgentType::Product
        ));
    }

    #[test]
    fn test_name_to_type_unknown() {
        assert!(matches!(
            ConfigurableAgent::name_to_type("unknown"),
            AgentType::Custom(_)
        ));
    }

    #[test]
    fn test_name_to_type_custom_preserves_name() {
        if let AgentType::Custom(name) = ConfigurableAgent::name_to_type("my-custom-agent") {
            assert_eq!(name, "my-custom-agent");
        } else {
            panic!("Expected Custom variant");
        }
    }

    #[test]
    fn test_name_to_type_empty_string() {
        assert!(matches!(
            ConfigurableAgent::name_to_type(""),
            AgentType::Custom(ref s) if s.is_empty()
        ));
    }

    // ==========================================================
    //  4. Accessors
    // ==========================================================

    #[test]
    fn test_name_accessor() {
        let config = make_config(vec![], None);
        let agent = ConfigurableAgent::new("product", &config, Box::new(MockLLM::new()), None);
        assert_eq!(agent.name(), "product");
    }

    #[test]
    fn test_max_tool_iterations_accessor() {
        let mut config = make_config(vec![], None);
        config.max_tool_iterations = 42;
        let agent = ConfigurableAgent::new("router", &config, Box::new(MockLLM::new()), None);
        assert_eq!(agent.max_tool_iterations(), 42);
    }

    #[test]
    fn test_parallel_tools_accessor() {
        let mut config = make_config(vec![], None);
        config.parallel_tools = true;
        let agent = ConfigurableAgent::new("router", &config, Box::new(MockLLM::new()), None);
        assert!(agent.parallel_tools());
    }

    #[test]
    fn test_tools_returns_some_when_provided() {
        let config = make_config(vec![], None);
        let tools = Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new()));
        let agent = ConfigurableAgent::new(
            "router",
            &config,
            Box::new(MockLLM::new()),
            Some(tools.clone()),
        );
        assert!(agent.tools().is_some());
        assert!(Arc::ptr_eq(agent.tools().unwrap(), &tools));
    }

    #[test]
    fn test_tools_returns_none_when_absent() {
        let config = make_config(vec![], None);
        let agent = ConfigurableAgent::new("router", &config, Box::new(MockLLM::new()), None);
        assert!(agent.tools().is_none());
    }

    #[test]
    fn test_allowed_tools_from_config() {
        let config = make_config(vec!["calculator", "web_search"], None);
        let agent = ConfigurableAgent::new("orchestrator", &config, Box::new(MockLLM::new()), None);
        let allowed = agent.allowed_tools().expect("should have allowed tools");
        assert_eq!(allowed.len(), 2);
        assert!(allowed.contains(&"calculator".to_string()));
        assert!(allowed.contains(&"web_search".to_string()));
    }

    // ==========================================================
    //  5. can_use_tool
    // ==========================================================

    #[test]
    fn test_can_use_tool_in_allowed_but_no_registry() {
        let config = make_config(vec!["calculator"], None);
        let agent = ConfigurableAgent::new("router", &config, Box::new(MockLLM::new()), None);
        assert!(!agent.can_use_tool("calculator"), "no registry → false");
    }

    #[test]
    fn test_can_use_tool_not_in_allowed_list() {
        let config = make_config(vec!["calculator"], None);
        let reg = Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new()));
        let agent = ConfigurableAgent::new("router", &config, Box::new(MockLLM::new()), Some(reg));
        assert!(!agent.can_use_tool("web_search"), "not in allowed → false");
    }

    #[test]
    fn test_can_use_tool_both_empty() {
        let config = make_config(vec![], None);
        let agent = ConfigurableAgent::new("router", &config, Box::new(MockLLM::new()), None);
        assert!(!agent.can_use_tool("anything"));
    }

    // ==========================================================
    //  6. get_filtered_tool_definitions
    // ==========================================================

    #[test]
    fn test_get_filtered_tool_definitions_no_registry() {
        let config = make_config(vec!["calculator"], None);
        let agent = ConfigurableAgent::new("router", &config, Box::new(MockLLM::new()), None);
        assert!(agent.get_filtered_tool_definitions().is_empty());
    }

    #[test]
    fn test_get_filtered_tool_definitions_with_registry() {
        let reg = make_tools_with_tool("calculator");
        let config = make_config(vec!["calculator"], None);
        let agent = ConfigurableAgent::new("router", &config, Box::new(MockLLM::new()), Some(reg));
        let defs = agent.get_filtered_tool_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "calculator");
    }

    // ==========================================================
    //  7. with_params constructor
    // ==========================================================

    #[test]
    fn test_with_params_sets_all_fields() {
        let agent = ConfigurableAgent::with_params(
            "my-agent",
            AgentType::Finance,
            Box::new(MockLLM::new()),
            "Custom prompt".to_string(),
            Some(Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new()))),
            Some(vec!["tool_a".to_string()]),
            10,
            true,
        );
        assert_eq!(agent.name(), "my-agent");
        assert!(matches!(agent.agent_type(), AgentType::Finance));
        assert_eq!(agent.system_prompt(), "Custom prompt");
        assert!(agent.tools().is_some());
        let allowed = agent.allowed_tools().expect("should have allowed tools");
        assert_eq!(allowed.len(), 1);
        assert_eq!(agent.max_tool_iterations(), 10);
        assert!(agent.parallel_tools());
    }

    #[test]
    fn test_with_params_provider_name_is_unknown() {
        let agent = ConfigurableAgent::with_params(
            "x",
            AgentType::Router,
            Box::new(MockLLM::new()),
            "p".to_string(),
            None,
            None,
            1,
            false,
        );
        assert_eq!(agent.provider_name, "unknown");
    }

    // ==========================================================
    //  8. new_with_provider constructor
    // ==========================================================

    #[test]
    fn test_new_with_provider_uses_explicit_name() {
        let config = make_config(vec![], None);
        let agent = ConfigurableAgent::new_with_provider(
            "sales",
            &config,
            Box::new(MockLLM::new()),
            None,
            "my-provider".to_string(),
        );
        assert_eq!(agent.name(), "sales");
        assert!(matches!(agent.agent_type(), AgentType::Sales));
        assert_eq!(agent.provider_name, "my-provider");
    }

    #[test]
    fn test_new_with_provider_falls_back_to_default_prompt() {
        let config = make_config(vec![], None); // system_prompt: None
        let agent = ConfigurableAgent::new_with_provider(
            "invoice",
            &config,
            Box::new(MockLLM::new()),
            None,
            "p".to_string(),
        );
        assert!(
            agent.system_prompt().contains("Invoice"),
            "should use default_system_prompt for 'invoice'"
        );
    }

    #[test]
    fn test_new_with_provider_uses_config_prompt_when_some() {
        let config = make_config(vec![], Some("Custom system prompt"));
        let agent = ConfigurableAgent::new_with_provider(
            "invoice",
            &config,
            Box::new(MockLLM::new()),
            None,
            "p".to_string(),
        );
        assert_eq!(agent.system_prompt(), "Custom system prompt");
    }

    // ==========================================================
    //  9. Agent trait methods
    // ==========================================================

    #[test]
    fn test_agent_trait_system_prompt() {
        let config = make_config(vec![], Some("Hello from config"));
        let agent = ConfigurableAgent::new("router", &config, Box::new(MockLLM::new()), None);
        assert_eq!(Agent::system_prompt(&agent), "Hello from config");
    }

    #[test]
    fn test_agent_trait_agent_type() {
        let config = make_config(vec![], None);
        let agent = ConfigurableAgent::new("finance", &config, Box::new(MockLLM::new()), None);
        assert!(matches!(Agent::agent_type(&agent), AgentType::Finance));
    }

    // ==========================================================
    //  10. Agent::execute — simple path (no tools)
    // ==========================================================

    #[tokio::test]
    async fn test_execute_simple_calls_generate_with_history() {
        let config = make_config(vec![], Some("You are helpful"));
        let agent = ConfigurableAgent::new(
            "router",
            &config,
            Box::new(MockLLM::with_content("hello world")),
            None,
        );
        let ctx = make_context();
        let resp = Agent::execute(&agent, "hi", &ctx).await.unwrap();
        assert_eq!(resp.content, "hello world");
    }

    #[tokio::test]
    async fn test_execute_simple_returns_metadata() {
        let config = make_config(vec![], None);
        let agent = ConfigurableAgent::new(
            "router",
            &config,
            Box::new(MockLLM::with_content("ok")),
            None,
        );
        let ctx = make_context();
        let resp = Agent::execute(&agent, "test", &ctx).await.unwrap();
        let meta = resp.metadata.unwrap();
        assert_eq!(meta.model_name, "mock");
        assert_eq!(meta.provider_name, "default"); // from AgentConfig.model
    }

    #[tokio::test]
    async fn test_execute_simple_empty_conversation_history() {
        let config = make_config(vec![], Some("system"));
        let agent = ConfigurableAgent::new(
            "router",
            &config,
            Box::new(MockLLM::with_content("reply")),
            None,
        );
        let ctx = AgentContext {
            user_id: "u".to_string(),
            session_id: "s".to_string(),
            conversation_history: vec![],
            user_memory: None,
        };
        let resp = Agent::execute(&agent, "q", &ctx).await.unwrap();
        assert_eq!(resp.content, "reply");
    }

    #[tokio::test]
    async fn test_execute_simple_with_conversation_history() {
        let config = make_config(vec![], Some("system"));
        let agent = ConfigurableAgent::new(
            "router",
            &config,
            Box::new(MockLLM::with_content("contextual reply")),
            None,
        );
        let ctx = make_context_with_history(vec![
            (MessageRole::User, "first question"),
            (MessageRole::Assistant, "first answer"),
            (MessageRole::User, "follow up"),
        ]);
        let resp = Agent::execute(&agent, "final", &ctx).await.unwrap();
        assert_eq!(resp.content, "contextual reply");
    }

    #[tokio::test]
    async fn test_execute_simple_with_user_memory() {
        let config = make_config(vec![], Some("system"));
        let agent = ConfigurableAgent::new(
            "router",
            &config,
            Box::new(MockLLM::with_content("memory reply")),
            None,
        );
        let ctx = AgentContext {
            user_id: "u".to_string(),
            session_id: "s".to_string(),
            conversation_history: vec![],
            user_memory: Some(UserMemory {
                user_id: "u".to_string(),
                preferences: vec![Preference {
                    category: "communication".to_string(),
                    key: "style".to_string(),
                    value: "concise".to_string(),
                    confidence: 0.9,
                }],
                facts: vec![],
            }),
        };
        let resp = Agent::execute(&agent, "q", &ctx).await.unwrap();
        assert_eq!(resp.content, "memory reply");
    }

    // ==========================================================
    //  11. Agent::execute — tool path (with tools + registry)
    // ==========================================================

    #[tokio::test]
    async fn test_execute_tool_path_no_tool_calls_returns_final() {
        let reg = make_tools_with_tool("calculator");
        let mut config = make_config(vec!["calculator"], Some("system"));
        config.max_tool_iterations = 3;
        // MockLLM returns empty tool_calls → immediate return
        let agent = ConfigurableAgent::new(
            "orchestrator",
            &config,
            Box::new(MockLLM::with_content("final answer")),
            Some(reg),
        );
        let ctx = make_context();
        let resp = Agent::execute(&agent, "compute 2+2", &ctx).await.unwrap();
        assert_eq!(resp.content, "final answer");
        let meta = resp.metadata.unwrap();
        assert_eq!(meta.model_name, "mock");
    }

    #[tokio::test]
    async fn test_execute_tool_path_tool_calls_then_final() {
        let reg = make_tools_with_tool("calculator");
        let mut config = make_config(vec!["calculator"], Some("system"));
        config.max_tool_iterations = 3;

        // First call: return a tool_call; second call: return final content
        let tool_call = ToolCall {
            id: "tc_1".to_string(),
            name: "calculator".to_string(),
            arguments: serde_json::json!({"expression": "2+2"}),
        };
        let responses = vec![
            make_tool_response("", vec![tool_call]),
            make_tool_response("The answer is 4", vec![]),
        ];

        let agent = ConfigurableAgent::new(
            "orchestrator",
            &config,
            Box::new(MockLLM::with_tool_responses(responses)),
            Some(reg),
        );
        let ctx = make_context();
        let resp = Agent::execute(&agent, "2+2?", &ctx).await.unwrap();
        assert_eq!(resp.content, "The answer is 4");
    }

    #[tokio::test]
    async fn test_execute_tool_path_max_iterations_reaches_synthesis() {
        let reg = make_tools_with_tool("calculator");
        let mut config = make_config(vec!["calculator"], Some("system"));
        config.max_tool_iterations = 2; // low limit to trigger synthesis

        let tc = ToolCall {
            id: "tc_1".to_string(),
            name: "calculator".to_string(),
            arguments: serde_json::json!({}),
        };
        // Both calls return tool_calls → loop exhausts → synthesis call
        let responses = vec![
            make_tool_response("", vec![tc.clone()]),
            make_tool_response("", vec![tc]),
        ];

        let agent = ConfigurableAgent::new(
            "orchestrator",
            &config,
            Box::new(MockLLM::with_tool_responses(responses)),
            Some(reg),
        );
        let ctx = make_context();
        let resp = Agent::execute(&agent, "compute", &ctx).await.unwrap();
        // After max iterations, synthesis call returns default content "mock"
        // (queue exhausted → fallback)
        assert!(!resp.content.is_empty());
    }

    #[tokio::test]
    async fn test_execute_tool_path_tool_execution_error() {
        // Register a tool that always errors
        struct FailingTool;
        #[async_trait]
        impl ares_tools::Tool for FailingTool {
            fn name(&self) -> &str {
                "fail_tool"
            }
            fn description(&self) -> &str {
                "always fails"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(&self, _args: serde_json::Value) -> Result<serde_json::Value> {
                Err(ares_types::AppError::Internal("tool crashed".to_string()))
            }
        }

        let reg = Arc::new(Tools::from_static([
            Arc::new(FailingTool) as Arc<dyn Tool>,
        ]));

        let mut config = make_config(vec!["fail_tool"], Some("system"));
        config.max_tool_iterations = 3;

        let tc = ToolCall {
            id: "tc_err".to_string(),
            name: "fail_tool".to_string(),
            arguments: serde_json::json!({}),
        };
        let responses = vec![
            make_tool_response("", vec![tc]),
            make_tool_response("Error handled", vec![]),
        ];

        let agent = ConfigurableAgent::new(
            "orchestrator",
            &config,
            Box::new(MockLLM::with_tool_responses(responses)),
            Some(reg),
        );
        let ctx = make_context();
        let resp = Agent::execute(&agent, "do it", &ctx).await.unwrap();
        // The tool error is caught and returned as a tool result JSON;
        // the LLM then produces a final response.
        assert_eq!(resp.content, "Error handled");
    }

    // ==========================================================
    //  12. Runtime allowed_tools enforcement (DIR1-46)
    // ==========================================================

    #[test]
    fn test_can_use_tool_empty_list_denies_all() {
        let reg = Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new()));
        let agent = ConfigurableAgent::with_params(
            "router",
            AgentType::Router,
            Box::new(MockLLM::new()),
            "system".to_string(),
            Some(reg),
            Some(vec![]),
            1,
            false,
        );
        assert!(
            !agent.can_use_tool("anything"),
            "empty allowed_tools → deny all"
        );
    }

    #[test]
    fn prebuilt_connector_tool_detection_covers_registered_connectors() {
        assert!(is_prebuilt_connector_tool("slack_send_message"));
        assert!(is_prebuilt_connector_tool("google_calendar_list_events"));
        assert!(is_prebuilt_connector_tool("salesforce_create_record"));
        assert!(!is_prebuilt_connector_tool("calculator"));
    }

    #[tokio::test]
    async fn dispatch_prebuilt_connector_injects_runtime_tenant() {
        let reg = make_tools_with_echo_tool("slack_send_message");
        let mut agent = ConfigurableAgent::with_params(
            "orchestrator",
            AgentType::Orchestrator,
            Box::new(MockLLM::new()),
            "system".to_string(),
            Some(reg),
            Some(vec!["slack_send_message".to_string()]),
            3,
            false,
        );
        let ctx = crate::tenant_scope(&cordis::Context::new_root(), "tenant-a");
        agent.bind_request_ctx(ctx);

        let result = agent
            .dispatch_tool("slack_send_message", serde_json::json!({"channel":"ops"}))
            .await
            .expect("connector dispatch");

        assert_eq!(result["tenant_id"], "tenant-a");
        assert_eq!(result["channel"], "ops");
    }

    #[tokio::test]
    async fn dispatch_prebuilt_connector_rejects_cross_tenant_arg() {
        let reg = make_tools_with_echo_tool("slack_send_message");
        let mut agent = ConfigurableAgent::with_params(
            "orchestrator",
            AgentType::Orchestrator,
            Box::new(MockLLM::new()),
            "system".to_string(),
            Some(reg),
            Some(vec!["slack_send_message".to_string()]),
            3,
            false,
        );
        let ctx = crate::tenant_scope(&cordis::Context::new_root(), "tenant-a");
        agent.bind_request_ctx(ctx);

        let err = agent
            .dispatch_tool(
                "slack_send_message",
                serde_json::json!({"tenant_id":"tenant-b","channel":"ops"}),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("does not match executing tenant"));
    }

    #[tokio::test]
    async fn test_execute_tool_allowed_tool_succeeds() {
        let reg = make_tools_with_tool("http");
        let mut config = make_config(vec!["http"], Some("system"));
        config.max_tool_iterations = 3;

        let tc = ToolCall {
            id: "tc_1".to_string(),
            name: "http".to_string(),
            arguments: serde_json::json!({}),
        };
        let responses = vec![
            make_tool_response("", vec![tc]),
            make_tool_response("HTTP result", vec![]),
        ];

        let agent = ConfigurableAgent::new(
            "orchestrator",
            &config,
            Box::new(MockLLM::with_tool_responses(responses)),
            Some(reg),
        );
        let ctx = make_context();
        let resp = Agent::execute(&agent, "fetch", &ctx).await.unwrap();
        assert_eq!(resp.content, "HTTP result");
    }

    #[tokio::test]
    async fn test_execute_tool_disallowed_tool_returns_error() {
        let reg = Arc::new(Tools::from_static([
            Arc::new(MockTool::new("http")) as Arc<dyn Tool>,
            Arc::new(MockTool::new("sql")) as Arc<dyn Tool>,
        ]));

        let agent = ConfigurableAgent::with_params(
            "orchestrator",
            AgentType::Orchestrator,
            Box::new(MockLLM::with_tool_responses(vec![make_tool_response(
                "",
                vec![ToolCall {
                    id: "tc_1".to_string(),
                    name: "sql".to_string(),
                    arguments: serde_json::json!({}),
                }],
            )])),
            "system".to_string(),
            Some(reg),
            Some(vec!["http".to_string()]),
            3,
            false,
        );

        let ctx = make_context();
        let result = Agent::execute(&agent, "query", &ctx).await;
        assert!(result.is_err(), "disallowed tool should return error");
        let err = result.err().unwrap().to_string();
        assert!(
            err.contains("sql"),
            "error should mention denied tool: {}",
            err
        );
        assert!(
            err.contains("not allowed"),
            "error should say tool is not allowed: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_execute_tool_empty_allowed_tools_denies_all() {
        let reg = make_tools_with_tool("http");
        let agent = ConfigurableAgent::with_params(
            "orchestrator",
            AgentType::Orchestrator,
            Box::new(MockLLM::with_tool_responses(vec![make_tool_response(
                "",
                vec![ToolCall {
                    id: "tc_1".to_string(),
                    name: "http".to_string(),
                    arguments: serde_json::json!({}),
                }],
            )])),
            "system".to_string(),
            Some(reg),
            Some(vec![]),
            3,
            false,
        );

        let ctx = make_context();
        let result = Agent::execute(&agent, "fetch", &ctx).await;
        assert!(result.is_err(), "empty allowed_tools should deny all");
        let err = result.err().unwrap().to_string();
        assert!(
            err.contains("http"),
            "error should mention denied tool: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_execute_tool_none_allowed_tools_denies_all() {
        let reg = Arc::new(Tools::from_static([
            Arc::new(MockTool::new("http")) as Arc<dyn Tool>,
            Arc::new(MockTool::new("sql")) as Arc<dyn Tool>,
        ]));

        let agent = ConfigurableAgent::with_params(
            "orchestrator",
            AgentType::Orchestrator,
            Box::new(MockLLM::with_tool_responses(vec![
                make_tool_response(
                    "",
                    vec![ToolCall {
                        id: "tc_1".to_string(),
                        name: "sql".to_string(),
                        arguments: serde_json::json!({}),
                    }],
                ),
                make_tool_response("SQL result", vec![]),
            ])),
            "system".to_string(),
            Some(reg),
            None,
            3,
            false,
        );

        let ctx = make_context();
        let result = Agent::execute(&agent, "query", &ctx).await;
        assert!(result.is_err(), "missing allowed_tools should deny all");
        let err = result.err().unwrap().to_string();
        assert!(
            err.contains("sql"),
            "error should mention denied tool: {err}"
        );
    }

    #[test]
    fn test_observed_tool_type_marks_builtins() {
        let agent = ConfigurableAgent::with_params(
            "router",
            AgentType::Router,
            Box::new(MockLLM::new()),
            "system".to_string(),
            None,
            Some(vec!["http".to_string()]),
            1,
            false,
        );

        assert_eq!(agent.observed_tool_type("http", true), "builtin");
    }

    #[test]
    fn test_observed_tool_type_falls_back_for_runtime_tools() {
        let agent = ConfigurableAgent::with_params(
            "router",
            AgentType::Router,
            Box::new(MockLLM::new()),
            "system".to_string(),
            None,
            Some(vec!["tenant_http".to_string()]),
            1,
            false,
        );

        assert_eq!(agent.observed_tool_type("tenant_http", false), "runtime");
    }

    #[test]
    fn test_set_allowed_tools_intersection() {
        let reg = Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new()));
        let mut agent = ConfigurableAgent::with_params(
            "router",
            AgentType::Router,
            Box::new(MockLLM::new()),
            "system".to_string(),
            Some(reg),
            Some(vec!["http".to_string(), "sql".to_string()]),
            1,
            false,
        );
        assert!(agent.can_use_tool("http"));
        assert!(agent.can_use_tool("sql"));
        agent.set_allowed_tools(Some(vec!["http".to_string()]));
        assert!(agent.can_use_tool("http"));
        assert!(!agent.can_use_tool("sql"));
        agent.set_allowed_tools(Some(vec![]));
        assert!(!agent.can_use_tool("http"));
        agent.set_allowed_tools(None);
        assert!(!agent.can_use_tool("sql"));
    }

    // ============== Fallback tests ==============

    struct FailingMockLLM {
        error_msg: String,
    }

    #[async_trait]
    impl LLMClient for FailingMockLLM {
        async fn generate(&self, _: &str) -> Result<String> {
            Err(ares_types::types::AppError::LLM(self.error_msg.clone()))
        }
        async fn generate_with_system(&self, _: &str, _: &str) -> Result<String> {
            Err(ares_types::types::AppError::LLM(self.error_msg.clone()))
        }
        async fn generate_with_history(&self, _: &[(String, String)]) -> Result<LLMResponse> {
            Err(ares_types::types::AppError::LLM(self.error_msg.clone()))
        }
        async fn generate_with_tools(&self, _: &str, _: &[ToolDefinition]) -> Result<LLMResponse> {
            Err(ares_types::types::AppError::LLM(self.error_msg.clone()))
        }
        async fn generate_with_tools_and_history(
            &self,
            _: &[ares_llm::coordinator::ConversationMessage],
            _: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            Err(ares_types::types::AppError::LLM(self.error_msg.clone()))
        }
        async fn stream(
            &self,
            _: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }
        async fn stream_with_system(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }
        async fn stream_with_history(
            &self,
            _: &[(String, String)],
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }
        fn model_name(&self) -> &str {
            "failing-mock"
        }
    }

    #[tokio::test]
    async fn test_fallback_used_when_primary_fails() {
        let mut agent = ConfigurableAgent::with_params(
            "test",
            AgentType::Product,
            Box::new(FailingMockLLM {
                error_msg: "primary failed".to_string(),
            }),
            "system".to_string(),
            None,
            None,
            1,
            false,
        );

        let fallback = MockLLM::with_content("fallback-success");
        agent.set_fallback_llms_with_providers(vec![(
            "fallback-provider".to_string(),
            Box::new(fallback),
        )]);

        let ctx = make_context();
        let resp = Agent::execute(&agent, "hello", &ctx).await.unwrap();
        assert_eq!(resp.content, "fallback-success");
        let metadata = resp.metadata.expect("metadata");
        assert_eq!(metadata.provider_name, "fallback-provider");
        assert_eq!(metadata.model_name, "mock");
    }

    #[tokio::test]
    async fn test_primary_succeeds_without_fallback() {
        let mut agent = ConfigurableAgent::with_params(
            "test",
            AgentType::Product,
            Box::new(MockLLM::with_content("primary-success")),
            "system".to_string(),
            None,
            None,
            1,
            false,
        );

        let fallback = FailingMockLLM {
            error_msg: "fallback should not run".to_string(),
        };
        agent.set_fallback_llms(vec![Box::new(fallback)]);

        let ctx = make_context();
        let resp = Agent::execute(&agent, "hello", &ctx).await.unwrap();
        assert_eq!(resp.content, "primary-success");
    }

    #[tokio::test]
    async fn test_all_fallbacks_fail_reports_every_error() {
        let mut agent = ConfigurableAgent::with_params(
            "test",
            AgentType::Product,
            Box::new(FailingMockLLM {
                error_msg: "primary failed".to_string(),
            }),
            "system".to_string(),
            None,
            None,
            1,
            false,
        );

        agent.set_fallback_llms(vec![
            Box::new(FailingMockLLM {
                error_msg: "fallback-0 failed".to_string(),
            }),
            Box::new(FailingMockLLM {
                error_msg: "fallback-1 failed".to_string(),
            }),
        ]);

        let ctx = make_context();
        let err = match Agent::execute(&agent, "hello", &ctx).await {
            Ok(_) => panic!("expected all LLMs to fail"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("primary failed"), "got: {err}");
        assert!(err.contains("fallback[0]"), "got: {err}");
        assert!(err.contains("fallback-0 failed"), "got: {err}");
        assert!(err.contains("fallback[1]"), "got: {err}");
        assert!(err.contains("fallback-1 failed"), "got: {err}");
    }

    #[test]
    fn user_id_from_ctx_reads_intercept() {
        use ares_types::models::{TenantContext, TenantTier};

        let root: Arc<cordis::Context> = cordis::Context::new_root();
        assert_eq!(crate::user_id_from_ctx(&root, ""), "");

        let ctx = root.with_intercept(TenantContext::new("acme".into(), TenantTier::Pro));
        assert_eq!(crate::user_id_from_ctx(&ctx, "anon"), "acme");
    }

    #[test]
    fn user_id_from_ctx_isolate_wins_over_intercept() {
        use ares_types::models::{TenantContext, TenantTier};

        let root: Arc<cordis::Context> = cordis::Context::new_root();
        let intercepted =
            root.with_intercept(TenantContext::new("from-intercept".into(), TenantTier::Pro));
        let isolated = crate::tenant_scope(&intercepted, "from-isolate");
        assert_eq!(crate::user_id_from_ctx(&isolated, "anon"), "from-isolate");
    }

    #[tokio::test]
    async fn configurable_generate_waterfall_rewrites_last_message() {
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());
        events.on_waterfall("llm.generate".into(), |mut payload, next| async move {
            if let Some(arr) = payload.get_mut("messages").and_then(|v| v.as_array_mut()) {
                if let Some(last) = arr.last_mut() {
                    last["content"] = serde_json::json!("rewritten-hello");
                }
            }
            next(payload).await
        });

        let mut agent = ConfigurableAgent::new(
            "router",
            &make_config(vec![], Some("system")),
            Box::new(MockLLM::echo_last()),
            None,
        );
        agent.bind_request_ctx(ctx);

        let resp = Agent::execute(&agent, "original", &make_context())
            .await
            .expect("execute");
        assert_eq!(resp.content, "rewritten-hello");
    }

    #[tokio::test]
    async fn configurable_generate_short_circuit_skips_llm() {
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());
        events.on_waterfall("llm.generate".into(), |_payload, _next| async move {
            Ok(serde_json::json!({ "content": "cached" }))
        });

        let (llm, generated) = MockLLM::with_generated_flag();
        let mut agent = ConfigurableAgent::new(
            "router",
            &make_config(vec![], Some("system")),
            Box::new(llm),
            None,
        );
        agent.bind_request_ctx(ctx);

        let resp = Agent::execute(&agent, "would-call-llm", &make_context())
            .await
            .expect("execute");
        assert_eq!(resp.content, "cached");
        assert!(
            !generated.load(Ordering::SeqCst),
            "dummy generate must stay false when handler skips next"
        );
    }
}
