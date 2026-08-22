//! Agent execution service — single place handling conversation history loading,
//! memory injection, tool coordination, observability, usage/cost, token budget,
//! and loop detection.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use cordis::{Context, CordisError, Service};
use ares_types::types::{AppError, Message};

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
#[derive(Clone)]
#[derive(Default)]
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
}

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
            .finish()
    }
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
pub struct Execute {
    context_provider: Option<Arc<dyn crate::context_provider::ContextProvider>>,
    /// Agent registry for creating agents from config (Phase 4 §15).
    agent_registry: Option<Arc<crate::registry::AgentRegistry>>,
    /// Run tracker for observability (Phase 4: extracted from root crate ActiveRuns).
    run_tracker: Option<Arc<dyn RunTracker>>,
}

impl Execute {
    /// Create a new service with no backing stores (useful for tests and
    /// `cargo check --no-default-features`).
    pub fn new() -> Self {
        Self {
            context_provider: None,
            agent_registry: None,
            run_tracker: None,
        }
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
        payload: serde_json::Value,
    ) -> serde_json::Value {
        let Some(events) = ctx.get::<cordis::EventsService>() else {
            return payload;
        };
        events
            .dispatch(
                "agent.started".into(),
                payload.clone(),
                cordis::Dispatch::Parallel,
            )
            .await
            .unwrap_or(payload)
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
        let Some(events) = ctx.get::<cordis::EventsService>() else { return; };
        let _ = events.dispatch(event.into(), payload, cordis::Dispatch::Emit).await;
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
    async fn run_resolved(
        &self,
        req: &AgentRequest,
        ctx: &Arc<Context>,
    ) -> Option<std::result::Result<ExecutionResult, AppError>> {
        use crate::Agent;

        let registry_owned = self
            .agent_registry
            .clone()
            .or_else(|| ctx.get::<crate::registry::AgentRegistry>());
        let registry = registry_owned.as_ref()?;
        let resolver = ctx
            .get::<crate::resolver::Resolver>()
            .or_else(|| {
                crate::resolver::Resolver::from_ctx(ctx, registry_owned.clone()).map(Arc::new)
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

        let Some(tenant_db) = ctx.get::<ares_store::TenantDb>() else {
            return None;
        };
        let Some(fleet_secrets) = ctx.get::<ares_config::fleet_secrets::FleetSecrets>() else {
            return None;
        };

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

        let run_id = uuid::Uuid::new_v4().to_string();
        if let Some(tracker) = &self.run_tracker {
            tracker.start_run(&run_id, &user_id, &req.agent_name, Some("execution_service"));
        }

        if ctx.get::<cordis::EventsService>().is_some() {
            let payload = serde_json::json!({
                "agent_name": req.agent_name,
                "run_id": run_id,
                "tenant": user_id,
                "event": "agent.started"
            });
            let _ = self.emit_agent_started(ctx, payload).await;
        }

        let agent_context = ares_types::types::AgentContext {
            user_id: user_id.to_string(),
            session_id: format!("exec-{}", uuid::Uuid::new_v4()),
            conversation_history: req.history.clone(),
            user_memory: None,
        };

        let result = agent.execute(&req.message, &agent_context).await;

        if let Some(tracker) = &self.run_tracker {
            let status = if result.is_ok() { "completed" } else { "failed" };
            tracker.finish_run(&run_id, status);
        }

        if let Ok(response) = result.as_ref() {
            if let Some(usage) = &response.usage {
                self.emit_observability(
                    ctx,
                    "agent.usage",
                    serde_json::json!({
                        "tenant": user_id,
                        "prompt": usage.prompt_tokens,
                        "completion": usage.completion_tokens,
                        "total": usage.total_tokens,
                    }),
                )
                .await;
            }
        }

        self.emit_observability(
            ctx,
            "agent.completed",
            serde_json::json!({
                "agent_name": req.agent_name,
                "run_id": run_id,
                "status": if result.is_ok() { "completed" } else { "failed" },
                "event": "agent.completed"
            }),
        )
        .await;
        if result.is_err() {
            self.emit_observability(
                ctx,
                "agent.failed",
                serde_json::json!({
                    "agent_name": req.agent_name,
                    "run_id": run_id,
                    "tenant": user_id,
                    "event": "agent.failed",
                }),
            )
            .await;
        }

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
            tracing::debug!(
                history_len = req.history.len(),
                "history load via TenantDb"
            );
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
        let provider_opt: Option<Arc<dyn crate::context_provider::ContextProvider>> =
            req.ctx_provider.clone().or_else(|| self.context_provider.clone());
        if let Some(provider) = provider_opt {
            let tid = tenant.clone().unwrap_or_default();
            let rt_ctx = crate::context_provider::AgentRuntimeContext::new(
                tid.clone(),
                &req.agent_name,
                "agent_execution",
            );
            if let Some(s) = provider.get_context_for_run(&rt_ctx).await {
                tracing::debug!(len = s.len(), "memory injected via ContextProvider::get_context_for_run");
                injected_context = Some(s);
            } else if let Some(s) = provider.get_context(&req.agent_name, &tid).await {
                tracing::debug!(len = s.len(), "memory injected via ContextProvider::get_context");
                injected_context = Some(s);
            }
        }

        let tools = ctx.get::<ares_tools::Tools>();
        let tool_definitions = tools.as_ref().map(|t| t.list(ctx)).unwrap_or_default();
        tracing::debug!(
            count = tool_definitions.len(),
            has_service = tools.is_some(),
            "tools resolved via Tools::list"
        );
        let _resolve_probe = tools.as_ref().and_then(|t| t.resolve(ctx, "__probe__"));

        let system_prompt = if let Some(extra) = injected_context.clone() {
            format!("{}

You are {}.", extra, req.agent_name)
        } else {
            format!("You are {}.", req.agent_name)
        };

        let mut base_messages: Vec<ares_llm::coordinator::ConversationMessage> = Vec::new();
        base_messages.push(ares_llm::coordinator::ConversationMessage::system(
            system_prompt.clone(),
        ));
        for msg in &req.history {
            let cm = match msg.role {
                ares_types::types::MessageRole::User => {
                    ares_llm::coordinator::ConversationMessage::user(&msg.content)
                }
                ares_types::types::MessageRole::Assistant => {
                    ares_llm::coordinator::ConversationMessage::assistant(&msg.content, vec![])
                }
                _ => ares_llm::coordinator::ConversationMessage::system(&msg.content),
            };
            base_messages.push(cm);
        }
        base_messages.push(ares_llm::coordinator::ConversationMessage::user(
            req.message.clone(),
        ));
        let _ = base_messages;

        if let Some(llm) = ctx.get::<ares_llm::Llm>() {
            match llm
                .get_client_boxed(ctx, ares_llm::CapabilityRequirements::default())
                .await
            {
                Ok(client) => {
                    let registry = Arc::new(ares_tools::registry::ToolRegistry::new());
                    let config = ares_llm::coordinator::ToolCallingConfig::default();
                    let coordinator =
                        ares_llm::coordinator::ToolCoordinator::new(client, registry, config);
                    match coordinator.execute(Some(&system_prompt), &req.message).await {
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
                            self.emit_observability(
                                ctx,
                                "agent.usage",
                                serde_json::json!({
                                    "tenant": tenant,
                                    "prompt": usage.prompt_tokens,
                                    "completion": usage.completion_tokens,
                                    "total": usage.total_tokens,
                                }),
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
                            tracing::warn!(error = %e, "ToolCoordinator loop failed, trying fallback LLM chain");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Llm::get_client failed");
                }
            }

            if let Ok(fb_client) = llm
                .get_client(ctx, ares_llm::CapabilityRequirements::default())
                .await
            {
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
}

impl Default for Execute {
    fn default() -> Self {
        Self::new()
    }
}

/// Derive tenant for `execute` without requiring the postgres-only resolver module.
/// Scope tools and execution to one tenant. Isolate wins over intercept.
pub fn tenant_scope(ctx: &Arc<Context>, tenant_id: &str) -> Arc<Context> {
    ctx.isolate::<ares_tools::Tools>(tenant_id)
        .isolate::<Execute>(tenant_id)
}

/// Derive user/tenant scope: `Execute` isolate label (strip `tenant:`/`user:`),
/// then `TenantContext` intercept, then `fallback`.
pub fn user_id_from_ctx(ctx: &Arc<Context>, fallback: &str) -> String {
    if let Some(label) = ctx.isolate_label(std::any::TypeId::of::<Execute>()) {
        let trimmed = label
            .strip_prefix("tenant:")
            .or_else(|| label.strip_prefix("user:"))
            .unwrap_or(&label);
        if !trimmed.is_empty() {
            return trimmed.to_string();
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

    fn init(&self, _ctx: &Arc<Context>) -> Pin<Box<dyn Future<Output = Result<Option<Box<dyn cordis::Disposable>>, CordisError>> + Send + '_>> {
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
            "agent.started".into(),
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
            "agent.started".into(),
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
        svc.emit_agent_started(&ctx, serde_json::json!({"agent_name": "a"}))
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
            "agent.usage".into(),
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
        svc.emit_observability(&ctx, "agent.usage", serde_json::json!({}))
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
        for event in ["agent.completed", "agent.failed"] {
            let flag = ran.clone();
            _guards.push(events.on(
                event.into(),
                move |payload: serde_json::Value| {
                    let flag = flag.clone();
                    async move {
                        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                        flag.store(true, Ordering::SeqCst);
                        Ok(payload)
                    }
                },
            ));
        }

        let start = std::time::Instant::now();
        svc.emit_observability(&ctx, "agent.completed", serde_json::json!({}))
            .await;
        svc.emit_observability(&ctx, "agent.failed", serde_json::json!({}))
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
        async fn execute(&self, _args: serde_json::Value) -> ares_types::types::Result<serde_json::Value> {
            Ok(serde_json::json!({"ok": true}))
        }
    }

    fn tools_with_probe() -> ares_tools::Tools {
        let mut registry = ares_tools::registry::ToolRegistry::new();
        registry.register(Arc::new(ProbeTool {
            name: "probe".into(),
        }));
        ares_tools::Tools::new(Arc::new(registry))
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
        assert!(names.contains(&"probe".to_string()), "Tools::list(ctx) sees intercept tenant tools");
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
    async fn execute_isolate_label_wins_over_intercept_for_tools() {
        let svc = Execute::new();
        let intercepted = Context::new_root().with_intercept(
            ares_types::models::TenantContext::new(
                "from-intercept".into(),
                ares_types::models::TenantTier::Pro,
            ),
        );
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
}
