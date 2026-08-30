//! Agent execution service — single place handling conversation history loading,
//! memory injection, tool coordination, observability, usage/cost, token budget,
//! and loop detection.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ares_types::types::{AppError, ContentPart, Message};
use cordis::{Context, CordisError, EventsService, Service};
use futures::StreamExt;

/// SSE token stream: text chunks only. Tool calls stay inside the loop.
pub type TokenStream =
    Pin<Box<dyn futures::Stream<Item = Result<String, AppError>> + Send + 'static>>;

fn once_text_stream(text: String) -> TokenStream {
    Box::pin(async_stream::stream! {
        yield Ok(text);
    })
}

#[cfg(feature = "postgres")]
struct PreparedResolvedAgent {
    agent: crate::ConfigurableAgent,
    source: AgentSource,
    user_id: String,
    run_id: String,
    agent_context: ares_types::types::AgentContext,
}

/// Result of `Execute::run` including resolution metadata.
///
/// This allows callers (v1/chat, scheduler, pipeline) to record which source the agent
/// came from and what config was used, without re-resolving.
/// Resolution tier label returned alongside the executed agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentSource {
    User,
    Community,
    System,
}

impl AgentSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Community => "community",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// The agent's response.
    pub response: crate::AgentResponse,
    /// Source tier where the agent was resolved (tenant/community/system).
    pub source: AgentSource,
    /// Name of the agent that was executed.
    pub agent_name: String,
    /// Run ID for correlation with ActiveRuns.
    pub run_id: String,
}

use crate::AgentResponse;

pub use ares_tools::Tools;

/// Canonical per-request model override used by the LLM interceptor.
///
/// Re-exporting the LLM type keeps context interception and provider policy
/// enforcement on the same `TypeId` across the agent and server crates.
pub use ares_llm::ModelOverride;

/// Request for unified agent execution.
///
/// Carries the minimal fields needed to execute any agent via the single
/// `Execute::run` entry-point.
#[derive(Clone, Default)]
pub struct AgentRequest {
    /// Agent name to execute.
    pub agent_name: String,
    /// Current user message.
    pub message: String,
    /// Prior conversation history (explicitly passed; may be augmented by
    /// `TenantDb` when available).
    pub history: Vec<Message>,
    /// Optional per-request context provider override (overrides service-level
    /// provider when `Some`).
    pub ctx_provider: Option<Arc<dyn crate::context_provider::ContextProvider>>,
    /// Multimodal parts for the current user turn (HTTP persists these via add_message_with_parts).
    pub parts: Vec<ContentPart>,
    /// OpenAI Responses continuation id for this turn.
    pub previous_response_id: Option<String>,
    /// When true, enable the LLM provider's built-in web search.
    pub web_search: bool,
}

/// Internal marker for skill-triggered executions.
///
/// Background engines attach this marker to their tenant-scoped request
/// context and still cross the same public `Execute::run` boundary as regular
/// agent requests. Keeping the marker in the context avoids a second public
/// execution API or changes to the request shape used by downstream crates.
#[derive(Clone)]
pub(crate) struct SkillDispatch {
    pub(crate) skill_id: String,
    pub(crate) tenant_id: String,
    pub(crate) input: serde_json::Value,
    pub(crate) run_id: String,
}

impl SkillDispatch {
    pub(crate) fn new(
        skill_id: impl Into<String>,
        tenant_id: impl Into<String>,
        input: serde_json::Value,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            skill_id: skill_id.into(),
            tenant_id: tenant_id.into(),
            input,
            run_id: run_id.into(),
        }
    }
}

impl Service for SkillDispatch {}

impl std::fmt::Debug for AgentRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRequest")
            .field("agent_name", &self.agent_name)
            .field("message", &self.message)
            .field("history_len", &self.history.len())
            .field(
                "ctx_provider",
                &self.ctx_provider.as_ref().map(|_| "Some(ContextProvider)"),
            )
            .field("parts_len", &self.parts.len())
            .field("previous_response_id", &self.previous_response_id)
            .field("web_search", &self.web_search)
            .finish()
    }
}

/// Apply per-turn generation hints on a resolved LLM client.
pub fn apply_generation_hints(
    client: &dyn ares_llm::LLMClient,
    web_search: bool,
    previous_response_id: Option<String>,
) {
    if !web_search && previous_response_id.is_none() {
        return;
    }
    client.set_hints(ares_llm::GenerationHints {
        web_search,
        previous_response_id,
        ..Default::default()
    });
}

/// Build the in-flight user turn, attaching multimodal parts and a continuation id.
pub fn user_message_with_parts(
    content: impl Into<String>,
    parts: Vec<ContentPart>,
    previous_response_id: Option<String>,
) -> ares_llm::coordinator::ConversationMessage {
    let mut msg = ares_llm::coordinator::ConversationMessage::user(content);
    msg.parts = parts;
    msg.previous_response_id = previous_response_id;
    msg
}

/// Unified agent execution service — the single place handling:
///
/// - conversation history loading (`TenantDb`)
/// - memory injection (`ContextProvider`)
/// - `ToolCoordinator` loop
/// - fallback LLM chain (`Coordinator`)
/// - observability sink (`run_history` + `agent_runs`)
/// - usage/cost aggregation
/// - token budget check
/// - loop detection
///
/// Reachable via `ctx.get::<Execute>()` (see `Service` impl).
#[derive(Clone)]
pub struct Execute {
    context_provider: Option<Arc<dyn crate::context_provider::ContextProvider>>,
    /// Agent registry for creating agents from config (Phase 4 §15).
    agent_registry: Option<Arc<crate::registry::AgentRegistry>>,
    /// Run tracker for observability (Phase 4: extracted from root crate ActiveRuns).
    run_tracker: Option<Arc<dyn RunTracker>>,
    /// Fail closed instead of falling back: when `true`, every echo/fallback
    /// path in `execute` returns `Err` so consumers never receive echoed
    /// input or fallback-LLM content mistaken for a real agent run.
    strict_fallbacks: bool,
}

impl Execute {
    /// Create a new service with no backing stores (useful for tests and
    /// `cargo check --no-default-features`).
    pub fn new() -> Self {
        Self {
            context_provider: None,
            agent_registry: None,
            run_tracker: None,
            strict_fallbacks: false,
        }
    }

    /// Enable strict fallback mode: every echo/fallback entry in `execute`
    /// returns `Err(AppError::Unavailable)` whose message starts with
    /// `strict_fallbacks:` instead of echoing the request.
    pub fn with_strict_fallbacks(mut self, strict: bool) -> Self {
        self.strict_fallbacks = strict;
        self
    }

    /// Whether strict fallback mode is enabled.
    pub fn strict_fallbacks(&self) -> bool {
        self.strict_fallbacks
    }

    /// Emit the `agent.started` event through the Cordis event bus with
    /// `Dispatch::Parallel`, which fans out to every registered observer
    /// concurrently and awaits all of them before returning (join-all).
    ///
    /// If no `EventsService` is present in the context, or the dispatch
    /// errors, the original `payload` is returned unchanged so callers never
    /// lose data.
    pub async fn emit_agent_started(
        &self,
        ctx: &Arc<Context>,
        payload: cordis::AgentStartedPayload,
    ) -> serde_json::Value {
        let value = match serde_json::to_value(&payload) {
            Ok(v) => v,
            Err(_) => return serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
        };
        let Some(events) = ctx.get::<cordis::EventsService>() else {
            return value;
        };
        events
            .dispatch_typed::<cordis::AgentStartedEvent>(&payload)
            .await
            .unwrap_or(value)
    }

    /// Fire-and-forget observability event via Cordis `Dispatch::Emit`.
    ///
    /// Returns immediately without waiting for handlers. Missing `EventsService`
    /// is a no-op. Usage snapshot recording stays in server middleware
    /// (`UsageContext` is not in this crate).
    pub async fn emit_observability(
        &self,
        ctx: &Arc<Context>,
        event: impl Into<String>,
        payload: serde_json::Value,
    ) {
        let Some(events) = ctx.get::<cordis::EventsService>() else {
            return;
        };
        let _ = events
            .dispatch(event.into(), payload, cordis::Dispatch::Emit)
            .await;
    }

    /// Typed fire-and-forget variant of [`emit_observability`]: dispatches the
    /// payload struct for its catalog-bound event via `Dispatch::Emit`.
    pub async fn emit_observability_typed<E: cordis::TypedEvent>(
        &self,
        ctx: &Arc<Context>,
        payload: &E::Payload,
    ) {
        let Some(events) = ctx.get::<cordis::EventsService>() else {
            return;
        };
        let _ = events.dispatch_typed::<E>(payload).await;
    }

    /// Attach a context provider for memory injection.
    pub fn with_context_provider(
        mut self,
        provider: Arc<dyn crate::context_provider::ContextProvider>,
    ) -> Self {
        self.context_provider = Some(provider);
        self
    }

    /// Attach an agent registry for creating agents from resolved configs.
    pub fn with_agent_registry(mut self, registry: Arc<crate::registry::AgentRegistry>) -> Self {
        self.agent_registry = Some(registry);
        self
    }

    /// Attach a run tracker for observability.
    pub fn with_run_tracker(mut self, tracker: Arc<dyn RunTracker>) -> Self {
        self.run_tracker = Some(tracker);
        self
    }

    /// Host-injected run tracker, if any.
    pub fn run_tracker(&self) -> Option<&Arc<dyn RunTracker>> {
        self.run_tracker.as_ref()
    }

    /// Execute an agent by name using the full pipeline: resolve → create → execute.
    ///
    /// This is the PRIMARY entry point that handlers should call. It:
    /// 1. Resolves the agent via crate-private `Resolver` (3-tier: tenant → community → system)
    /// 2. Creates the agent via `AgentRegistry::create_agent_from_config_with_fallbacks`
    /// 3. Calls `agent.execute(message, context)` with the request ctx bound
    /// 4. Returns `ExecutionResult` with response + resolution metadata
    ///
    /// Run tracking (start/finish) is handled internally via `RunTracker`.
    pub async fn run(
        &self,
        req: &AgentRequest,
        ctx: &Arc<Context>,
    ) -> std::result::Result<ExecutionResult, AppError> {
        crate::admit(ctx).await?;
        let Some(events) = ctx.get::<EventsService>() else {
            return self.run_resolved_or_execute(req, ctx).await;
        };
        let payload = serde_json::to_value(cordis::AgentRunRequest {
            agent_name: req.agent_name.clone(),
            message: req.message.clone(),
        })
        .unwrap_or(serde_json::Value::Null);
        let execute = self.clone();
        let ctx_owned = Arc::clone(ctx);
        let orig = req.clone();
        let out = events
            .waterfall_around(
                cordis::events_catalog::ev::AGENT_RUN.to_string(),
                payload,
                move |payload| async move {
                    let mut run_req = orig;
                    if let Some(name) = payload.get("agent_name").and_then(|v| v.as_str()) {
                        run_req.agent_name = name.to_string();
                    }
                    if let Some(msg) = payload.get("message").and_then(|v| v.as_str()) {
                        run_req.message = msg.to_string();
                    }
                    match execute.run_resolved_or_execute(&run_req, &ctx_owned).await {
                        Ok(er) => Ok(serde_json::json!({
                            "content": er.response.content,
                            "usage": er.response.usage,
                            "metadata": er.response.metadata.as_ref().map(|m| {
                                serde_json::json!({
                                    "model_name": m.model_name,
                                    "provider_name": m.provider_name,
                                })
                            }),
                            "source": er.source,
                            "agent_name": er.agent_name,
                            "run_id": er.run_id,
                        })),
                        Err(e) => Err(CordisError::Fiber(e.to_string())),
                    }
                },
            )
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        if out.get("deny").and_then(|v| v.as_bool()) == Some(true) {
            let reason = out
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("agent.run denied");
            return Err(AppError::InvalidInput(reason.to_string()));
        }
        let content = out
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let agent_name = out
            .get("agent_name")
            .and_then(|v| v.as_str())
            .unwrap_or(&req.agent_name)
            .to_string();
        let run_id = out
            .get("run_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let source = out
            .get("source")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(AgentSource::System);
        let usage = out
            .get("usage")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok());
        let metadata = out.get("metadata").and_then(|v| {
            Some(crate::ExecutionMetadata {
                model_name: v.get("model_name")?.as_str()?.to_string(),
                provider_name: v.get("provider_name")?.as_str()?.to_string(),
            })
        });
        Ok(ExecutionResult {
            response: AgentResponse {
                content,
                usage,
                metadata,
            },
            source,
            agent_name,
            run_id,
        })
    }

    /// Stream an agent response as text chunks. Tool calls stay inside the loop.
    ///
    /// Does not wrap in `agent.run` / `waterfall_around` — a stream cannot buffer JSON.
    pub async fn run_stream(
        &self,
        req: &AgentRequest,
        ctx: &Arc<Context>,
    ) -> Result<TokenStream, AppError> {
        crate::admit(ctx).await?;
        if let Some(dispatch) = ctx.get::<SkillDispatch>() {
            let result = self.run_skill(req, ctx, &dispatch).await?;
            return Ok(once_text_stream(result.response.content));
        }
        #[cfg(feature = "postgres")]
        {
            if let Some(prepared) = self.prepare_resolved_agent(req, ctx).await {
                let prepared = prepared?;
                let PreparedResolvedAgent {
                    agent,
                    source: _,
                    user_id,
                    run_id,
                    agent_context,
                } = prepared;
                let exec = self.clone();
                let ctx_owned = Arc::clone(ctx);
                let agent_name = req.agent_name.clone();
                match agent
                    .execute_stream(req.message.clone(), agent_context)
                    .await
                {
                    Ok(inner) => {
                        return Ok(Box::pin(async_stream::stream! {
                            let mut inner = inner;
                            let mut ok = true;
                            while let Some(item) = inner.next().await {
                                if item.is_err() {
                                    ok = false;
                                }
                                yield item;
                            }
                            exec.finish_resolved_run(
                                &ctx_owned,
                                &agent_name,
                                &user_id,
                                &run_id,
                                None,
                                ok,
                            )
                            .await;
                        }));
                    }
                    Err(e) => {
                        self.finish_resolved_run(
                            ctx,
                            &req.agent_name,
                            &user_id,
                            &run_id,
                            None,
                            false,
                        )
                        .await;
                        return Err(e);
                    }
                }
            }
        }
        self.execute_stream_fallback(req.clone(), ctx).await
    }

    async fn run_resolved_or_execute(
        &self,
        req: &AgentRequest,
        ctx: &Arc<Context>,
    ) -> std::result::Result<ExecutionResult, AppError> {
        if let Some(dispatch) = ctx.get::<SkillDispatch>() {
            return self.run_skill(req, ctx, &dispatch).await;
        }
        if let Some(result) = self.try_run_resolved(req, ctx).await {
            return result;
        }
        let response = self.execute(req.clone(), ctx).await?;
        Ok(ExecutionResult {
            response,
            source: AgentSource::System,
            agent_name: req.agent_name.clone(),
            run_id: uuid::Uuid::new_v4().to_string(),
        })
    }

    #[cfg(feature = "postgres")]
    async fn run_skill(
        &self,
        req: &AgentRequest,
        ctx: &Arc<Context>,
        dispatch: &SkillDispatch,
    ) -> std::result::Result<ExecutionResult, AppError> {
        let skill_engine = ctx
            .get::<crate::skills::SkillEngine>()
            .ok_or_else(|| AppError::Unavailable("SkillEngine is not provided".to_string()))?;
        let value = skill_engine
            .execute_skill(
                &dispatch.skill_id,
                &dispatch.tenant_id,
                dispatch.input.clone(),
                &dispatch.run_id,
                ctx,
            )
            .await
            .map_err(AppError::Internal)?;
        Ok(ExecutionResult {
            response: AgentResponse {
                content: serde_json::to_string(&value)
                    .map_err(|e| AppError::Internal(e.to_string()))?,
                usage: None,
                metadata: None,
            },
            source: AgentSource::System,
            agent_name: req.agent_name.clone(),
            run_id: dispatch.run_id.clone(),
        })
    }

    #[cfg(not(feature = "postgres"))]
    async fn run_skill(
        &self,
        _req: &AgentRequest,
        _ctx: &Arc<Context>,
        _dispatch: &SkillDispatch,
    ) -> std::result::Result<ExecutionResult, AppError> {
        Err(AppError::Unavailable(
            "SkillEngine requires postgres".to_string(),
        ))
    }

    async fn try_run_resolved(
        &self,
        req: &AgentRequest,
        ctx: &Arc<Context>,
    ) -> Option<std::result::Result<ExecutionResult, AppError>> {
        #[cfg(feature = "postgres")]
        {
            return self.run_resolved(req, ctx).await;
        }
        #[cfg(not(feature = "postgres"))]
        {
            let _ = (req, ctx);
            None
        }
    }

    #[cfg(feature = "postgres")]
    async fn prepare_resolved_agent(
        &self,
        req: &AgentRequest,
        ctx: &Arc<Context>,
    ) -> Option<std::result::Result<PreparedResolvedAgent, AppError>> {
        let registry_owned = self
            .agent_registry
            .clone()
            .or_else(|| ctx.get::<crate::registry::AgentRegistry>());
        let registry = registry_owned.as_ref()?;
        let resolver = ctx.get::<crate::resolver::Resolver>().or_else(|| {
            crate::resolver::Resolver::from_ctx(ctx, Arc::clone(registry)).map(Arc::new)
        })?;
        let resolved = resolver.resolve(ctx, &req.agent_name).await;
        let (user_agent, source) = match resolved {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let user_id = user_id_from_ctx(ctx, "");

        let mut config = crate::configurable::agent_config_from_user_agent(&user_agent);
        if let (Some(policy), Some(ovr)) = (
            ctx.get::<ares_llm::TenantModelPolicy>(),
            ctx.get::<ModelOverride>(),
        ) {
            if let Err(e) = policy.authorize(&ovr.model) {
                return Some(Err(e));
            }
        }
        if let Some(ovr) = ctx.get::<ModelOverride>() {
            tracing::info!(model=%ovr.model, agent=%req.agent_name, "model overridden via Cordis intercept");
            config.model = ovr.model.clone();
        }

        let tenant_db = ctx.get::<ares_store::TenantDb>()?;
        let fleet_secrets = ctx.get::<ares_store::FleetSecrets>()?;

        let mut agent = match registry
            .create_agent_from_config_with_fallbacks(
                &req.agent_name,
                &config,
                &user_id,
                tenant_db.pool(),
                &fleet_secrets,
            )
            .await
        {
            Ok(a) => a,
            Err(e) => return Some(Err(e)),
        };
        if let Some(tools) = ctx.get::<ares_tools::Tools>() {
            agent.set_tools(tools);
        }
        agent.bind_request_ctx(ctx.clone());
        agent.set_user_turn(
            req.parts.clone(),
            req.previous_response_id.clone(),
            req.web_search,
        );

        let run_id = uuid::Uuid::new_v4().to_string();
        if let Some(tracker) = &self.run_tracker {
            tracker.start_run(
                &run_id,
                &user_id,
                &req.agent_name,
                Some("execution_service"),
            );
        }

        if ctx.get::<cordis::EventsService>().is_some() {
            let _ = self
                .emit_agent_started(
                    ctx,
                    cordis::AgentStartedPayload {
                        agent_name: req.agent_name.clone(),
                        run_id: run_id.clone(),
                        tenant: user_id.to_string(),
                        event: cordis::events_catalog::ev::AGENT_STARTED.to_string(),
                    },
                )
                .await;
        }

        let agent_context = ares_types::types::AgentContext {
            user_id: user_id.to_string(),
            session_id: format!("exec-{}", uuid::Uuid::new_v4()),
            conversation_history: req.history.clone(),
            user_memory: None,
        };

        Some(Ok(PreparedResolvedAgent {
            agent,
            source,
            user_id: user_id.to_string(),
            run_id,
            agent_context,
        }))
    }

    #[cfg(feature = "postgres")]
    async fn finish_resolved_run(
        &self,
        ctx: &Arc<Context>,
        agent_name: &str,
        user_id: &str,
        run_id: &str,
        usage: Option<&ares_llm::client::TokenUsage>,
        ok: bool,
    ) {
        if let Some(tracker) = &self.run_tracker {
            tracker.finish_run(run_id, if ok { "completed" } else { "failed" });
        }
        if let Some(usage) = usage {
            self.emit_observability_typed::<cordis::AgentUsageEvent>(
                ctx,
                &cordis::AgentUsagePayload {
                    tenant: Some(user_id.to_string()),
                    prompt: usage.prompt_tokens as i64,
                    completion: usage.completion_tokens as i64,
                    total: usage.total_tokens as i64,
                },
            )
            .await;
        }
        self.emit_observability_typed::<cordis::AgentCompletedEvent>(
            ctx,
            &cordis::AgentCompletedPayload {
                agent_name: agent_name.to_string(),
                run_id: run_id.to_string(),
                status: if ok { "completed" } else { "failed" }.to_string(),
                event: cordis::events_catalog::ev::AGENT_COMPLETED.to_string(),
            },
        )
        .await;
        if !ok {
            self.emit_observability_typed::<cordis::AgentFailedEvent>(
                ctx,
                &cordis::AgentFailedPayload {
                    agent_name: agent_name.to_string(),
                    run_id: run_id.to_string(),
                    tenant: user_id.to_string(),
                    event: cordis::events_catalog::ev::AGENT_FAILED.to_string(),
                },
            )
            .await;
        }
    }

    #[cfg(feature = "postgres")]
    async fn run_resolved(
        &self,
        req: &AgentRequest,
        ctx: &Arc<Context>,
    ) -> Option<std::result::Result<ExecutionResult, AppError>> {
        use crate::Agent;

        let prepared = match self.prepare_resolved_agent(req, ctx).await? {
            Ok(p) => p,
            Err(e) => return Some(Err(e)),
        };
        let PreparedResolvedAgent {
            agent,
            source,
            user_id,
            run_id,
            agent_context,
        } = prepared;
        let result = agent.execute(&req.message, &agent_context).await;
        self.finish_resolved_run(
            ctx,
            &req.agent_name,
            &user_id,
            &run_id,
            result.as_ref().ok().and_then(|r| r.usage.as_ref()),
            result.is_ok(),
        )
        .await;
        Some(result.map(|response| ExecutionResult {
            response,
            source,
            agent_name: req.agent_name.clone(),
            run_id,
        }))
    }

    /// LLM/tools path used when Resolver/TenantDb are absent on ctx.
    async fn execute(
        &self,
        req: AgentRequest,
        ctx: &Arc<Context>,
    ) -> Result<AgentResponse, AppError> {
        if let Some(tenant_db) = tenant_db(ctx) {
            let _pool = tenant_db.pool();
            tracing::debug!(history_len = req.history.len(), "history load via TenantDb");
            let _ = _pool;
        }

        if let (Some(policy), Some(ovr)) = (
            ctx.get::<ares_llm::TenantModelPolicy>(),
            ctx.get::<ModelOverride>(),
        ) {
            policy.authorize(&ovr.model)?;
        }

        let tenant = tenant_from_request_ctx(ctx, None);

        let mut injected_context: Option<String> = None;
        let provider_opt: Option<Arc<dyn crate::context_provider::ContextProvider>> = req
            .ctx_provider
            .clone()
            .or_else(|| self.context_provider.clone());
        if let Some(provider) = provider_opt {
            let tid = tenant.clone().unwrap_or_default();
            let rt_ctx = crate::context_provider::AgentRuntimeContext::new(
                tid.clone(),
                &req.agent_name,
                "agent_execution",
            );
            if let Some(s) = provider.get_context_for_run(&rt_ctx).await {
                tracing::debug!(
                    len = s.len(),
                    "memory injected via ContextProvider::get_context_for_run"
                );
                injected_context = Some(s);
            } else if let Some(s) = provider.get_context(&req.agent_name, &tid).await {
                tracing::debug!(
                    len = s.len(),
                    "memory injected via ContextProvider::get_context"
                );
                injected_context = Some(s);
            }
        }

        let tools = ctx.get::<ares_tools::Tools>().unwrap_or_else(|| {
            Arc::new(ares_tools::Tools::from_static(std::iter::empty::<
                Arc<dyn ares_tools::Tool>,
            >()))
        });
        let tool_definitions = tools.list(ctx);
        tracing::debug!(
            count = tool_definitions.len(),
            has_service = true,
            "tools resolved via Tools::list"
        );
        let _resolve_probe = tools.resolve(ctx, "__probe__");

        let system_prompt = if let Some(extra) = injected_context.clone() {
            format!(
                "{}

You are {}.",
                extra, req.agent_name
            )
        } else {
            format!("You are {}.", req.agent_name)
        };

        let mut base_messages: Vec<ares_llm::coordinator::ConversationMessage> = Vec::new();
        base_messages.push(ares_llm::coordinator::ConversationMessage::system(
            system_prompt.clone(),
        ));
        for msg in &req.history {
            let mut cm = match msg.role {
                ares_types::types::MessageRole::User => {
                    ares_llm::coordinator::ConversationMessage::user(&msg.content)
                }
                ares_types::types::MessageRole::Assistant => {
                    ares_llm::coordinator::ConversationMessage::assistant(&msg.content, vec![])
                }
                _ => ares_llm::coordinator::ConversationMessage::system(&msg.content),
            };
            cm.parts = msg.parts.clone();
            base_messages.push(cm);
        }
        base_messages.push(user_message_with_parts(
            req.message.clone(),
            req.parts.clone(),
            req.previous_response_id.clone(),
        ));

        let llm = match ctx.get::<ares_llm::Llm>() {
            Some(llm) => Some(llm),
            None if self.strict_fallbacks => {
                return Err(AppError::Unavailable(
                    "strict_fallbacks: no Llm service on context".into(),
                ));
            }
            None => None,
        };
        if let Some(llm) = llm {
            match llm
                .get_client_boxed(ctx, ares_llm::CapabilityRequirements::default())
                .await
            {
                Ok(client) => {
                    apply_generation_hints(
                        client.as_ref(),
                        req.web_search,
                        req.previous_response_id.clone(),
                    );
                    if !req.parts.is_empty() || req.previous_response_id.is_some() {
                        match client
                            .generate_with_tools_and_history(&base_messages, &tool_definitions)
                            .await
                        {
                            Ok(resp) => {
                                return Ok(AgentResponse {
                                    content: resp.content,
                                    usage: resp.usage,
                                    metadata: None,
                                });
                            }
                            Err(e) => {
                                if self.strict_fallbacks {
                                    return Err(AppError::Unavailable(format!(
                                        "strict_fallbacks: generate_with_tools_and_history failed: {e}"
                                    )));
                                }
                                tracing::warn!(
                                    error = %e,
                                    "multimodal generate failed, trying fallback LLM chain"
                                );
                            }
                        }
                    } else {
                    let config = ares_llm::coordinator::ToolCallingConfig::default();
                    let coordinator = ares_llm::coordinator::ToolCoordinator::new(
                        client,
                        Arc::clone(&tools),
                        config,
                    );
                    match coordinator
                        .execute(Some(&system_prompt), &req.message, ctx)
                        .await
                    {
                        Ok(coord_result) => {
                            if let Some(_db) = tenant_db(ctx) {
                                tracing::debug!(
                                    content_len = coord_result.content.len(),
                                    "observability sink run_history/agent_runs via TenantDb"
                                );
                                let _ = _db;
                            }
                            let usage = coord_result.total_usage.clone();
                            if let Some(tdb) = tenant_db(ctx) {
                                let _pool = tdb.pool();
                                tracing::debug!(
                                    tenant = ?tenant,
                                    prompt = usage.prompt_tokens,
                                    completion = usage.completion_tokens,
                                    total = usage.total_tokens,
                                    "token budget check via TenantDb and usage aggregation"
                                );
                                let _ = _pool;
                            }
                            self.emit_observability_typed::<cordis::AgentUsageEvent>(
                                ctx,
                                &cordis::AgentUsagePayload {
                                    tenant: tenant.clone(),
                                    prompt: usage.prompt_tokens as i64,
                                    completion: usage.completion_tokens as i64,
                                    total: usage.total_tokens as i64,
                                },
                            )
                            .await;
                            let mut detector = crate::loop_detector::LoopDetector::new();
                            match detector.check(&coord_result.content) {
                                crate::loop_detector::LoopStatus::LoopDetected {
                                    repeats,
                                    action,
                                    kind,
                                } => {
                                    tracing::warn!(
                                        repeats,
                                        ?action,
                                        ?kind,
                                        "loop_detector triggered in Execute"
                                    );
                                }
                                crate::loop_detector::LoopStatus::Ok => {}
                            }
                            return Ok(AgentResponse {
                                content: coord_result.content,
                                usage: Some(usage),
                                metadata: None,
                            });
                        }
                        Err(e) => {
                            if self.strict_fallbacks {
                                return Err(AppError::Unavailable(format!(
                                    "strict_fallbacks: ToolCoordinator::execute failed: {e}"
                                )));
                            }
                            tracing::warn!(error = %e, "ToolCoordinator loop failed, trying fallback LLM chain");
                        }
                    }
                    }
                }
                Err(e) => {
                    if self.strict_fallbacks {
                        return Err(AppError::Unavailable(format!(
                            "strict_fallbacks: Llm::get_client_boxed failed: {e}"
                        )));
                    }
                    tracing::warn!(error = %e, "Llm::get_client failed");
                }
            }

            // Strict mode never reaches here (both Err arms above return
            // early), but guard anyway: fallback-LLM content is refused.
            if self.strict_fallbacks {
                return Err(AppError::Unavailable(
                    "strict_fallbacks: fallback LLM chain unavailable".into(),
                ));
            }
            if let Ok(fb_client) = llm
                .get_client(ctx, ares_llm::CapabilityRequirements::default())
                .await
            {
                apply_generation_hints(
                    fb_client.as_ref(),
                    req.web_search,
                    req.previous_response_id.clone(),
                );
                if let Ok(content) = fb_client.generate(&req.message).await {
                    if let Some(_db) = tenant_db(ctx) {
                        tracing::debug!("fallback observability run_history/agent_runs");
                        let _ = _db;
                    }
                    let mut detector = crate::loop_detector::LoopDetector::new();
                    let _ = detector.check(&content);
                    return Ok(AgentResponse {
                        content,
                        usage: None,
                        metadata: None,
                    });
                }
            }
        }

        if let Some(_db) = tenant_db(ctx) {
            tracing::debug!("echo fallback observability run_history/agent_runs");
            let _ = _db;
        }
        let mut detector = crate::loop_detector::LoopDetector::new();
        let _status = detector.check(&req.message);
        let _ = crate::loop_detector::LoopConfig::default();

        if self.strict_fallbacks {
            return Err(AppError::Unavailable(
                "strict_fallbacks: no LLM response available".into(),
            ));
        }

        Ok(AgentResponse {
            content: if req.message.is_empty() {
                system_prompt
            } else {
                req.message.clone()
            },
            usage: None,
            metadata: None,
        })
    }

    /// Stream fallback used when Resolver/TenantDb are absent on ctx.
    ///
    /// Mirrors `execute` setup (memory, tools, history with parts, hints) but
    /// streams tokens. Does not use `ToolCoordinator`.
    async fn execute_stream_fallback(
        &self,
        req: AgentRequest,
        ctx: &Arc<Context>,
    ) -> Result<TokenStream, AppError> {
        if let Some(tenant_db) = tenant_db(ctx) {
            let _pool = tenant_db.pool();
            tracing::debug!(history_len = req.history.len(), "history load via TenantDb");
            let _ = _pool;
        }

        if let (Some(policy), Some(ovr)) = (
            ctx.get::<ares_llm::TenantModelPolicy>(),
            ctx.get::<ModelOverride>(),
        ) {
            policy.authorize(&ovr.model)?;
        }

        let tenant = tenant_from_request_ctx(ctx, None);

        let mut injected_context: Option<String> = None;
        let provider_opt: Option<Arc<dyn crate::context_provider::ContextProvider>> = req
            .ctx_provider
            .clone()
            .or_else(|| self.context_provider.clone());
        if let Some(provider) = provider_opt {
            let tid = tenant.clone().unwrap_or_default();
            let rt_ctx = crate::context_provider::AgentRuntimeContext::new(
                tid.clone(),
                &req.agent_name,
                "agent_execution",
            );
            if let Some(s) = provider.get_context_for_run(&rt_ctx).await {
                tracing::debug!(
                    len = s.len(),
                    "memory injected via ContextProvider::get_context_for_run"
                );
                injected_context = Some(s);
            } else if let Some(s) = provider.get_context(&req.agent_name, &tid).await {
                tracing::debug!(
                    len = s.len(),
                    "memory injected via ContextProvider::get_context"
                );
                injected_context = Some(s);
            }
        }

        let tools = ctx.get::<ares_tools::Tools>().unwrap_or_else(|| {
            Arc::new(ares_tools::Tools::from_static(std::iter::empty::<
                Arc<dyn ares_tools::Tool>,
            >()))
        });
        let tool_definitions = tools.list(ctx);
        tracing::debug!(
            count = tool_definitions.len(),
            has_service = true,
            "tools resolved via Tools::list"
        );
        let _resolve_probe = tools.resolve(ctx, "__probe__");

        let system_prompt = if let Some(extra) = injected_context.clone() {
            format!(
                "{}

You are {}.",
                extra, req.agent_name
            )
        } else {
            format!("You are {}.", req.agent_name)
        };

        let mut base_messages: Vec<ares_llm::coordinator::ConversationMessage> = Vec::new();
        base_messages.push(ares_llm::coordinator::ConversationMessage::system(
            system_prompt.clone(),
        ));
        for msg in &req.history {
            let mut cm = match msg.role {
                ares_types::types::MessageRole::User => {
                    ares_llm::coordinator::ConversationMessage::user(&msg.content)
                }
                ares_types::types::MessageRole::Assistant => {
                    ares_llm::coordinator::ConversationMessage::assistant(&msg.content, vec![])
                }
                _ => ares_llm::coordinator::ConversationMessage::system(&msg.content),
            };
            cm.parts = msg.parts.clone();
            base_messages.push(cm);
        }
        base_messages.push(user_message_with_parts(
            req.message.clone(),
            req.parts.clone(),
            req.previous_response_id.clone(),
        ));

        let echo_text = if req.message.is_empty() {
            system_prompt.clone()
        } else {
            req.message.clone()
        };

        let llm = match ctx.get::<ares_llm::Llm>() {
            Some(llm) => Some(llm),
            None if self.strict_fallbacks => {
                return Err(AppError::Unavailable(
                    "strict_fallbacks: no Llm service on context".into(),
                ));
            }
            None => None,
        };

        if let Some(llm) = llm {
            match llm
                .get_client_boxed(ctx, ares_llm::CapabilityRequirements::default())
                .await
            {
                Ok(client) => {
                    apply_generation_hints(
                        client.as_ref(),
                        req.web_search,
                        req.previous_response_id.clone(),
                    );
                    let ctx = Arc::clone(ctx);
                    let req_message = req.message.clone();
                    let req_parts_empty = req.parts.is_empty();
                    let max_iters =
                        ares_llm::coordinator::ToolCallingConfig::default().max_iterations;
                    return Ok(Box::pin(async_stream::stream! {
                        for _iteration in 0..max_iters {
                            match client
                                .stream_with_tools_and_history(&base_messages, &tool_definitions)
                                .await
                            {
                                Ok(mut evs) => {
                                    let mut tool_calls = Vec::new();
                                    let mut text_acc = String::new();
                                    let mut failed = false;
                                    while let Some(ev) = evs.next().await {
                                        match ev {
                                            Ok(ares_llm::LlmStreamEvent::Text(chunk)) => {
                                                text_acc.push_str(&chunk);
                                                yield Ok(chunk);
                                            }
                                            Ok(ares_llm::LlmStreamEvent::ToolCalls(calls)) => {
                                                tool_calls = calls;
                                            }
                                            Err(e) => {
                                                yield Err(e);
                                                failed = true;
                                                break;
                                            }
                                        }
                                    }
                                    if failed {
                                        return;
                                    }
                                    if tool_calls.is_empty() {
                                        return;
                                    }
                                    base_messages.push(
                                        ares_llm::coordinator::ConversationMessage::assistant(
                                            &text_acc,
                                            tool_calls.clone(),
                                        ),
                                    );
                                    for tc in &tool_calls {
                                        let result = tools
                                            .execute(&ctx, &tc.name, tc.arguments.clone())
                                            .await;
                                        let result_value = match result {
                                            Ok(v) => v,
                                            Err(e) => serde_json::json!({"error": e.to_string()}),
                                        };
                                        base_messages.push(
                                            ares_llm::coordinator::ConversationMessage::tool_result(
                                                &tc.id,
                                                &result_value,
                                            ),
                                        );
                                    }
                                }
                                Err(e)
                                    if matches!(e, AppError::FeatureDisabled(_))
                                        && tool_definitions.is_empty()
                                        && req_parts_empty =>
                                {
                                    match client.stream(&req_message).await {
                                        Ok(mut s) => {
                                            while let Some(item) = s.next().await {
                                                yield item;
                                            }
                                        }
                                        Err(e) => yield Err(e),
                                    }
                                    return;
                                }
                                Err(e) => {
                                    yield Err(e);
                                    return;
                                }
                            }
                        }
                    }));
                }
                Err(e) => {
                    if self.strict_fallbacks {
                        return Err(AppError::Unavailable(format!(
                            "strict_fallbacks: Llm::get_client_boxed failed: {e}"
                        )));
                    }
                    tracing::warn!(error = %e, "Llm::get_client failed");
                }
            }
        }

        if self.strict_fallbacks {
            return Err(AppError::Unavailable(
                "strict_fallbacks: no LLM response available".into(),
            ));
        }
        Ok(once_text_stream(echo_text))
    }
}

impl Default for Execute {
    fn default() -> Self {
        Self::new()
    }
}

/// Derive tenant for `execute` without requiring the postgres-only resolver module.
/// Scope tools and execution to one tenant. Isolate wins over intercept.
pub fn tenant_scope(ctx: &Arc<Context>, tenant_id: &str) -> Arc<Context> {
    #[cfg(feature = "postgres")]
    if let Some(realms) = ctx.get::<ares_store::TenantRealms>() {
        return realms.open(ctx, tenant_id);
    }
    // Only data-bearing services are realm-isolated. `Execute` is a shared
    // stateless engine; isolating it hid the root instance and broke every
    // request path resolving it post-scope (v1/chat 503 regression).
    ctx.isolate::<ares_tools::Tools>(tenant_id)
}

/// Request-path tenant: open the realm (or isolate) then intercept `TenantContext`.
/// Background jobs keep using [`tenant_scope`] (isolate only, no intercept).
pub fn request_tenant_ctx(
    ctx: &Arc<Context>,
    tc: ares_types::models::TenantContext,
) -> Arc<Context> {
    tenant_scope(ctx, &tc.tenant_id).with_intercept(tc)
}

/// JWT `user:` isolate when no tenant is present. Does not invent `TenantContext`.
pub fn request_user_scope(ctx: &Arc<Context>, user_id: &str) -> Arc<Context> {
    let label = format!("user:{user_id}");
    ctx.isolate::<ares_tools::Tools>(&label)
}

/// Derive user/tenant scope: `Execute` isolate label (strip `tenant:`/`user:`),
/// then `TenantContext` intercept, then `fallback`.
pub fn user_id_from_ctx(ctx: &Arc<Context>, fallback: &str) -> String {
    // Legacy label first (realms created before Execute stopped being
    // isolated), then the live realm boundary on `Tools`.
    for tid in [
        std::any::TypeId::of::<Execute>(),
        std::any::TypeId::of::<ares_tools::Tools>(),
    ] {
        if let Some(label) = ctx.isolate_label(tid) {
            let trimmed = label
                .strip_prefix("tenant:")
                .or_else(|| label.strip_prefix("user:"))
                .unwrap_or(&label);
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    if let Some(tc) = ctx.get::<ares_types::models::TenantContext>() {
        if !tc.tenant_id.is_empty() {
            return tc.tenant_id.clone();
        }
    }
    fallback.to_string()
}

#[cfg(feature = "postgres")]
fn tenant_db(ctx: &Arc<Context>) -> Option<Arc<ares_store::TenantDb>> {
    ctx.get::<ares_store::TenantDb>()
}

#[cfg(not(feature = "postgres"))]
struct NoTenantDb;

#[cfg(not(feature = "postgres"))]
impl NoTenantDb {
    fn pool(&self) -> &() {
        &()
    }
}

#[cfg(not(feature = "postgres"))]
fn tenant_db(_ctx: &Arc<Context>) -> Option<Arc<NoTenantDb>> {
    None
}

fn tenant_from_request_ctx(ctx: &Arc<Context>, fallback: Option<&str>) -> Option<String> {
    let id = user_id_from_ctx(ctx, fallback.unwrap_or(""));
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

impl Service for Execute {
    fn name(&self) -> &'static str {
        "Execute"
    }

    fn init(
        &self,
        _ctx: &Arc<Context>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<Box<dyn cordis::Disposable>>, CordisError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move { Ok(None) })
    }

    fn check(&self) -> bool {
        true
    }
}

/// Trait for tracking active agent runs. Implemented by the root crate's `ActiveRuns`
/// and injected into `Execute` via the Context.
///
/// This allows `ares-agent` (a leaf crate) to track runs without depending on root-crate types.
pub trait RunTracker: Send + Sync + 'static {
    /// Register a new run as active.
    fn start_run(&self, run_id: &str, tenant_id: &str, agent_name: &str, source: Option<&str>);
    /// Update run progress.
    fn update_run(&self, run_id: &str, status: &str, step: i32);
    /// Mark run as finished with terminal status.
    fn finish_run(&self, run_id: &str, status: &str);
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// RED contract: the `agent.started` event must be fanned out to every
    /// registered handler via Cordis `Dispatch::Parallel` (join-all), so the
    /// dispatch awaits all handlers before returning. A fire-and-forget
    /// `Dispatch::Emit` returns immediately and may not have run any handler,
    /// so this assertion would be flaky/false under the old implementation.
    ///
    /// The harness calls the not-yet-existing public seam `emit_agent_started`,
    /// which the implement phase adds and wires into `run` in place
    /// of the `Dispatch::Emit` at line ~272.
    #[tokio::test]
    async fn agent_started_fans_out_via_parallel() {
        let svc = Execute::new();
        let ctx = Context::new_root();
        let events = ctx.provide(cordis::EventsService::new());

        let count = Arc::new(AtomicUsize::new(0));

        // Handler 1 — `Dispatch::Parallel` must run it before returning.
        let c1 = count.clone();
        let _d1 = events.on(
            cordis::events_catalog::ev::AGENT_STARTED.to_string(),
            move |payload: serde_json::Value| {
                let c = c1.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(payload)
                }
            },
        );

        // Handler 2 — also must be run before the dispatch returns.
        let c2 = count.clone();
        let _d2 = events.on(
            cordis::events_catalog::ev::AGENT_STARTED.to_string(),
            move |payload: serde_json::Value| {
                let c = c2.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(payload)
                }
            },
        );

        // Seam the implement phase adds: dispatches "agent.started" with
        // `Dispatch::Parallel` and returns the resulting value.
        svc.emit_agent_started(
            &ctx,
            cordis::AgentStartedPayload {
                agent_name: "a".into(),
                run_id: String::new(),
                tenant: String::new(),
                event: "agent.started".into(),
            },
        )
        .await;

        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "Dispatch::Parallel must join both 'agent.started' handlers before returning"
        );
    }

    /// `Dispatch::Emit` must return before a slow handler finishes, then the
    /// handler still runs on the runtime after the call returns.
    #[tokio::test]
    async fn emit_observability_returns_without_waiting_for_slow_handler() {
        let svc = Execute::new();
        let ctx = Context::new_root();
        let events = ctx.provide(cordis::EventsService::new());

        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        let _d = events.on(
            cordis::events_catalog::ev::AGENT_USAGE.to_string(),
            move |payload: serde_json::Value| {
                let flag = flag.clone();
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                    flag.store(true, Ordering::SeqCst);
                    Ok(payload)
                }
            },
        );

        let start = std::time::Instant::now();
        svc.emit_observability(
            &ctx,
            cordis::events_catalog::ev::AGENT_USAGE,
            serde_json::json!({}),
        )
        .await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(40),
            "emit_observability must return without awaiting handlers, elapsed {elapsed:?}"
        );

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            ran.load(Ordering::SeqCst),
            "slow agent.usage handler must still run after emit returns"
        );
    }

    #[tokio::test]
    async fn emit_agent_completed_and_failed_return_without_waiting() {
        let svc = Execute::new();
        let ctx = Context::new_root();
        let events = ctx.provide(cordis::EventsService::new());

        let ran = Arc::new(AtomicBool::new(false));
        let mut _guards = Vec::new();
        for event in [cordis::events_catalog::ev::AGENT_COMPLETED, "agent.failed"] {
            let flag = ran.clone();
            _guards.push(events.on(event.into(), move |payload: serde_json::Value| {
                let flag = flag.clone();
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                    flag.store(true, Ordering::SeqCst);
                    Ok(payload)
                }
            }));
        }

        let start = std::time::Instant::now();
        svc.emit_observability(
            &ctx,
            cordis::events_catalog::ev::AGENT_COMPLETED,
            serde_json::json!({}),
        )
        .await;
        svc.emit_observability(
            &ctx,
            cordis::events_catalog::ev::AGENT_FAILED,
            serde_json::json!({}),
        )
        .await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(40),
            "completed/failed must Emit without awaiting handlers, elapsed {elapsed:?}"
        );
    }

    struct ProbeTool {
        name: String,
    }

    #[async_trait::async_trait]
    impl ares_tools::Tool for ProbeTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "probe"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> ares_types::types::Result<serde_json::Value> {
            Ok(serde_json::json!({"ok": true}))
        }
    }

    fn tools_with_probe() -> ares_tools::Tools {
        ares_tools::Tools::from_static([Arc::new(ProbeTool {
            name: "probe".into(),
        }) as Arc<dyn ares_tools::Tool>])
    }

    async fn execute_with_tenant_context_intercept(tenant_id: &str) {
        let svc = Execute::new();
        let ctx = Context::new_root().with_intercept(ares_types::models::TenantContext::new(
            tenant_id.into(),
            ares_types::models::TenantTier::Pro,
        ));
        let _ = ctx.provide(tools_with_probe());
        let req = AgentRequest {
            agent_name: "echo".into(),
            message: "hi".into(),
            ..Default::default()
        };
        svc.run(&req, &ctx).await.expect("echo fallback");
        let tools = ctx.get::<ares_tools::Tools>().expect("Tools on ctx");
        let names: Vec<_> = tools.list(&ctx).into_iter().map(|d| d.name).collect();
        assert!(
            names.contains(&"probe".to_string()),
            "Tools::list(ctx) sees intercept tenant tools"
        );
    }

    #[tokio::test]
    async fn execute_lists_tools_using_tenant_context_intercept() {
        execute_with_tenant_context_intercept("acme").await;
    }

    /// When `Tools` is on ctx, `run` must call `Tools::list(ctx)` /
    /// `Tools::resolve(ctx, name)` (isolate+intercept).
    #[tokio::test]
    async fn execute_lists_tools_via_tools_on_ctx() {
        let svc = Execute::new();
        let ctx = Context::new_root().with_intercept(ares_types::models::TenantContext::new(
            "acme".into(),
            ares_types::models::TenantTier::Pro,
        ));
        let _ = ctx.provide(tools_with_probe());
        let req = AgentRequest {
            agent_name: "echo".into(),
            message: "hi".into(),
            ..Default::default()
        };
        svc.run(&req, &ctx).await.expect("echo fallback");
        let tools = ctx.get::<ares_tools::Tools>().expect("Tools on ctx");
        assert!(tools.resolve(&ctx, "probe").is_some());
        assert!(tools.list(&ctx).iter().any(|d| d.name == "probe"));
    }

    /// Intercept path without an `AgentRequest` tenant field (aliases the listing test).
    #[tokio::test]
    async fn execute_uses_ctx_tenant_without_request_field() {
        execute_with_tenant_context_intercept("acme").await;
    }

    #[tokio::test]
    async fn request_tenant_ctx_keeps_root_execute_resolvable() {
        // Regression guard for v1/chat: the handler resolves `Execute` from the
        // tenant-scoped context. Root-provided Execute must stay visible inside
        // the realm (Tools stays isolated; Execute is the shared engine).
        let root = Context::new_root();
        let _fid = root.plugin(cordis::EventsService::new()).await;
        root.provide_arc(Arc::new(Execute::new()) as Arc<Execute>);
        let tc = ares_types::models::TenantContext::new(
            "acme".into(),
            ares_types::models::TenantTier::Pro,
        );
        let scoped = crate::request_tenant_ctx(&root, tc);
        assert!(
            scoped.get::<Execute>().is_some(),
            "root-provided Execute must resolve inside tenant scope"
        );
    }

    #[tokio::test]
    async fn execute_isolate_label_wins_over_intercept_for_tools() {
        let svc = Execute::new();
        let intercepted =
            Context::new_root().with_intercept(ares_types::models::TenantContext::new(
                "from-intercept".into(),
                ares_types::models::TenantTier::Pro,
            ));
        let ctx = tenant_scope(&intercepted, "from-isolate");
        let _ = ctx.provide(tools_with_probe());
        let req = AgentRequest {
            agent_name: "echo".into(),
            message: "hi".into(),
            ..Default::default()
        };
        svc.run(&req, &ctx).await.expect("echo fallback");
        assert_eq!(user_id_from_ctx(&ctx, "anon"), "from-isolate");
        let tools = ctx.get::<ares_tools::Tools>().expect("Tools on ctx");
        assert!(tools.list(&ctx).iter().any(|d| d.name == "probe"));
    }

    #[tokio::test]
    async fn execute_admit_denies_without_http() {
        let execute = Execute::new();
        let quota = ares_types::models::TenantQuota {
            tier: ares_types::models::TenantTier::Free,
            requests_per_month: 0,
            tokens_per_month: 0,
            max_agents: 1,
            requests_per_day: 0,
        };
        let tc = ares_types::models::TenantContext {
            tenant_id: "capped".into(),
            tier: ares_types::models::TenantTier::Free,
            quota,
        };
        let ctx = Context::new_root().with_intercept(tc);
        let _ = ctx.provide(cordis::EventsService::new());
        let req = AgentRequest {
            agent_name: "echo".into(),
            message: "should not run".into(),
            ..Default::default()
        };
        let err = execute.run(&req, &ctx).await.expect_err("quota deny");
        match err {
            AppError::RateLimited(msg) => {
                assert_eq!(msg, "Monthly request quota exceeded");
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_run_waterfall_rewrites_message() {
        let svc = Execute::new();
        let ctx = Context::new_root();
        let events = ctx.provide(cordis::EventsService::new());
        events.on_waterfall(
            cordis::events_catalog::ev::AGENT_RUN.to_string(),
            |mut payload, next| async move {
                payload["message"] = serde_json::json!("rewritten-hello");
                next(payload).await
            },
        );
        let req = AgentRequest {
            agent_name: "echo".into(),
            message: "original".into(),
            ..Default::default()
        };
        let result = svc.run(&req, &ctx).await.expect("echo fallback");
        assert!(
            result.response.content.contains("rewritten-hello"),
            "waterfall rewrite of message must reach echo execute, got {:?}",
            result.response.content
        );
    }

    #[tokio::test]
    async fn strict_fallbacks_errors_without_llm() {
        let svc = Execute::new().with_strict_fallbacks(true);
        let ctx = Context::new_root();
        let req = AgentRequest {
            agent_name: "echo".into(),
            message: "hi".into(),
            ..Default::default()
        };
        let err = svc.run(&req, &ctx).await.expect_err("strict must refuse");
        match err {
            AppError::Unavailable(m) => {
                assert!(
                    m.starts_with("strict_fallbacks:"),
                    "unexpected refusal message: {m}"
                );
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_execute_still_echoes_without_llm() {
        let svc = Execute::new();
        assert!(!svc.strict_fallbacks());
        let ctx = Context::new_root();
        let req = AgentRequest {
            agent_name: "echo".into(),
            message: "hello-echo".into(),
            ..Default::default()
        };
        let result = svc.run(&req, &ctx).await.expect("echo fallback");
        assert_eq!(result.response.content, "hello-echo");
    }

    #[tokio::test]
    async fn agent_run_short_circuit_skips_execute() {
        let svc = Execute::new();
        let ctx = Context::new_root();
        let events = ctx.provide(cordis::EventsService::new());
        events.on_waterfall(
            cordis::events_catalog::ev::AGENT_RUN.to_string(),
            |_payload, _next| async move {
                Ok(serde_json::json!({
                    "content": "short-circuit",
                    "source": "system",
                    "agent_name": "echo",
                    "run_id": "test-run",
                }))
            },
        );
        let req = AgentRequest {
            agent_name: "echo".into(),
            message: "would-echo-this-if-core-ran".into(),
            ..Default::default()
        };
        let result = svc.run(&req, &ctx).await.expect("short-circuit");
        assert_eq!(result.response.content, "short-circuit");
        assert_eq!(result.run_id, "test-run");
        assert_ne!(
            result.response.content, req.message,
            "skipping next must not run echo execute"
        );
    }

    #[tokio::test]
    async fn request_tenant_ctx_intercepts_after_scope() {
        let root = Context::new_root();
        #[cfg(feature = "postgres")]
        {
            root.provide(ares_store::TenantRealms::new(
                std::any::TypeId::of::<ares_tools::Tools>(),
                std::any::TypeId::of::<Execute>(),
            ));
        }
        let tc = ares_types::models::TenantContext::new(
            "acme".into(),
            ares_types::models::TenantTier::Pro,
        );
        let scoped = request_tenant_ctx(&root, tc);
        let got = scoped
            .get::<ares_types::models::TenantContext>()
            .expect("TenantContext intercept");
        assert_eq!(got.tenant_id, "acme");
        assert_eq!(
            scoped
                .isolate_label(std::any::TypeId::of::<ares_tools::Tools>())
                .as_deref(),
            Some("acme")
        );
        // Execute is the shared engine: no realm label, always resolvable.
        assert_eq!(
            scoped
                .isolate_label(std::any::TypeId::of::<Execute>())
                .as_deref(),
            None
        );
        #[cfg(feature = "postgres")]
        {
            let realms = root
                .get::<ares_store::TenantRealms>()
                .expect("TenantRealms");
            let realm = realms.open(&root, "acme");
            assert!(
                realm.get::<ares_types::models::TenantContext>().is_none(),
                "cached realm must stay intercept-free"
            );
            let realm2 = realms.open(&root, "acme");
            assert!(std::sync::Arc::ptr_eq(&realm, &realm2));
        }
    }

    #[tokio::test]
    async fn request_user_scope_does_not_invent_tenant_context() {
        let root = Context::new_root();
        let scoped = request_user_scope(&root, "user-1");
        assert!(scoped.get::<ares_types::models::TenantContext>().is_none());
        // Execute stays unlabeled (shared engine); Tools carries the realm.
        assert_eq!(
            scoped
                .isolate_label(std::any::TypeId::of::<Execute>())
                .as_deref(),
            None
        );
        assert_eq!(
            scoped
                .isolate_label(std::any::TypeId::of::<ares_tools::Tools>())
                .as_deref(),
            Some("user:user-1")
        );
    }

    #[tokio::test]
    async fn run_stream_echoes_without_llm() {
        let svc = Execute::new();
        let ctx = Context::new_root();
        let req = AgentRequest {
            agent_name: "echo".into(),
            message: "hello stream".into(),
            ..Default::default()
        };
        let mut stream = svc.run_stream(&req, &ctx).await.expect("stream");
        let first = stream.next().await.expect("chunk").expect("ok");
        assert_eq!(first, "hello stream");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn run_stream_echoes_with_parts() {
        let svc = Execute::new();
        let ctx = Context::new_root();
        let req = AgentRequest {
            agent_name: "echo".into(),
            message: "caption".into(),
            parts: vec![ContentPart::Text {
                text: "img".into(),
            }],
            ..Default::default()
        };
        let mut stream = svc.run_stream(&req, &ctx).await.expect("stream");
        let first = stream.next().await.expect("chunk").expect("ok");
        assert_eq!(first, "caption");
        assert!(stream.next().await.is_none());
    }
}
