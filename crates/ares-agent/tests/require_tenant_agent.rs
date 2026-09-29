//! AR-1 residuals (item 2.20): a request that sets `AgentRequest::require_tenant_agent`
//! never executes another agent, on the run path and on the stream path.
//!
//! Before this item, `Execute::run` / `Execute::run_stream` fell through to the generic
//! `execute` / `execute_stream_fallback` path whenever the resolved path returned `None`
//! (no agent registry, no `TenantDb`, no `FleetSecrets` on the context), and the tenant
//! tier was skipped altogether when the tenant id was empty (the resolver then ran the
//! same-named community or system agent). With the flag set, every such case must refuse
//! with a typed error.
//!
//! "The system agent did not run" is measured two ways, both must stay at zero:
//! - `llm_calls`: model calls made through the `Llm` service on the context, which is what
//!   the generic fall-through (`Execute::execute`) uses;
//! - `ollama_calls`: HTTP requests that reached the mock Ollama server, which is what an
//!   agent built from the registry (system tier) or from a tenant row uses.
//!
//! Every test needs the scratch database named by `TEST_DATABASE_URL`: `Execute::run` calls
//! `admit`, which reads usage through `TenantDb` whenever a `TenantContext` is present.
#![cfg(feature = "postgres")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ares_agent::{
    request_tenant_ctx, AgentConfig, AgentRegistry, AgentRequest, AgentSource, Execute,
};
use ares_llm::{
    LLMClient, LLMResponse, Llm, LlmStreamEvent, ModelConfig, ProviderConfig, ProviderRegistry,
};
use ares_store::tenant_agents::{create_tenant_agent, CreateTenantAgentRequest};
use ares_store::tenant_allowlist::TenantAllowlistStore;
use ares_store::{FleetSecrets, PostgresClient, TenantDb};
use ares_tools::{Tool, Tools};
use ares_types::models::{TenantContext, TenantTier};
use ares_types::types::{AppError, ToolDefinition};
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use cordis::Context;
use futures::StreamExt;
use serde_json::{json, Value};
use sqlx::PgPool;

/// The agent name used everywhere: it exists as a system agent in the registry.
const AGENT: &str = "product";
const SYSTEM_PROMPT: &str = "registry-product-prompt";
/// What the counting `Llm` answers with: proves the generic fall-through ran.
const FALLTHROUGH_ANSWER: &str = "generic-fallthrough-ran";

// ---------------------------------------------------------------------------
// A model client that counts every call.
// ---------------------------------------------------------------------------

struct CountingLlm {
    calls: Arc<AtomicUsize>,
}

impl CountingLlm {
    fn response(&self) -> LLMResponse {
        self.calls.fetch_add(1, Ordering::SeqCst);
        LLMResponse {
            content: FALLTHROUGH_ANSWER.to_string(),
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
            usage: None,
            reasoning_content: None,
            response_id: None,
        }
    }
}

type StringStream =
    Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>;

#[async_trait::async_trait]
impl LLMClient for CountingLlm {
    fn model_name(&self) -> &str {
        "counting-llm"
    }
    async fn generate(&self, _: &str) -> ares_types::types::Result<String> {
        Ok(self.response().content)
    }
    async fn generate_with_system(&self, _: &str, _: &str) -> ares_types::types::Result<String> {
        Ok(self.response().content)
    }
    async fn generate_with_history(
        &self,
        _: &[(String, String)],
    ) -> ares_types::types::Result<LLMResponse> {
        Ok(self.response())
    }
    async fn generate_with_tools(
        &self,
        _: &str,
        _: &[ToolDefinition],
    ) -> ares_types::types::Result<LLMResponse> {
        Ok(self.response())
    }
    async fn generate_with_tools_and_history(
        &self,
        _: &[ares_llm::coordinator::ConversationMessage],
        _: &[ToolDefinition],
    ) -> ares_types::types::Result<LLMResponse> {
        Ok(self.response())
    }
    async fn stream(&self, _: &str) -> ares_types::types::Result<StringStream> {
        let text = self.response().content;
        Ok(Box::new(futures::stream::iter(vec![
            Ok::<String, AppError>(text),
        ])))
    }
    async fn stream_with_system(
        &self,
        _: &str,
        _: &str,
    ) -> ares_types::types::Result<StringStream> {
        let text = self.response().content;
        Ok(Box::new(futures::stream::iter(vec![
            Ok::<String, AppError>(text),
        ])))
    }
    async fn stream_with_history(
        &self,
        _: &[(String, String)],
    ) -> ares_types::types::Result<StringStream> {
        let text = self.response().content;
        Ok(Box::new(futures::stream::iter(vec![
            Ok::<String, AppError>(text),
        ])))
    }
    async fn stream_with_tools_and_history(
        &self,
        _: &[ares_llm::coordinator::ConversationMessage],
        _: &[ToolDefinition],
    ) -> ares_types::types::Result<
        Box<dyn futures::Stream<Item = ares_types::types::Result<LlmStreamEvent>> + Send + Unpin>,
    > {
        let text = self.response().content;
        Ok(Box::new(futures::stream::iter(vec![Ok::<
            LlmStreamEvent,
            AppError,
        >(
            LlmStreamEvent::Text(text),
        )])))
    }
}

// ---------------------------------------------------------------------------
// A mock Ollama server that counts requests and echoes the system prompt.
// ---------------------------------------------------------------------------

async fn fake_ollama_chat(
    State(hits): State<Arc<AtomicUsize>>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    hits.fetch_add(1, Ordering::SeqCst);
    let system_prompt = payload["messages"]
        .as_array()
        .and_then(|messages| {
            messages.iter().find_map(|message| {
                (message.get("role").and_then(Value::as_str) == Some("system"))
                    .then(|| message.get("content").and_then(Value::as_str))
                    .flatten()
            })
        })
        .unwrap_or("missing-system-prompt");
    Json(json!({
        "model": payload["model"].as_str().unwrap_or("test-model"),
        "created_at": "2026-05-21T00:00:00Z",
        "message": {
            "role": "assistant",
            "content": format!("SYSTEM_PROMPT={}", system_prompt)
        },
        "done": true,
        "total_duration": 1,
        "load_duration": 1,
        "prompt_eval_count": 1,
        "prompt_eval_duration": 1,
        "eval_count": 1,
        "eval_duration": 1
    }))
}

async fn spawn_counting_ollama() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/api/chat", post(fake_ollama_chat))
        .with_state(Arc::clone(&hits));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock ollama");
    let addr = listener.local_addr().expect("mock ollama addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve mock ollama");
    });
    (format!("http://{addr}"), hits)
}

fn registry_with_system_agent(mock_ollama_url: &str) -> AgentRegistry {
    let mut provider_registry = ProviderRegistry::new();
    provider_registry.register_provider(
        "ollama-local",
        ProviderConfig::Ollama {
            api_key_env: "OLLAMA_API_KEY".to_string(),
            base_url: mock_ollama_url.to_string(),
            default_model: "mock-model".to_string(),
        },
    );
    provider_registry.register_model(
        "default",
        ModelConfig {
            provider: "ollama-local".to_string(),
            model: "mock-model".to_string(),
            temperature: 0.0,
            max_tokens: 512,
        },
    );
    let mut registry = AgentRegistry::new(
        Arc::new(provider_registry),
        Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new())),
    );
    registry.register(
        AGENT,
        AgentConfig {
            model: "default".to_string(),
            system_prompt: Some(SYSTEM_PROMPT.to_string()),
            tools: vec![],
            max_tool_iterations: 5,
            parallel_tools: false,
            compaction_enabled: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            extra: HashMap::new(),
            allowed_tools: None,
        },
    );
    registry
}

// ---------------------------------------------------------------------------
// The fixture: a context with every part present except the one named.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Missing {
    /// Everything present.
    Nothing,
    /// No `AgentRegistry` on the `Execute` service or the context.
    Registry,
    /// No `TenantDb` on the context.
    TenantDb,
    /// No `FleetSecrets` on the context.
    FleetSecrets,
    /// Registry, `TenantDb` and `FleetSecrets` all present, but the context carries no
    /// tenant: `user_id_from_ctx` yields "".
    TenantId,
}

struct Fixture {
    exec: Execute,
    ctx: Arc<Context>,
    pool: PgPool,
    /// The tenant the context is scoped to ("" when `Missing::TenantId`).
    tenant: String,
    /// Model calls through the `Llm` on the context (the generic fall-through).
    llm_calls: Arc<AtomicUsize>,
    /// Requests that reached the mock Ollama (a registry-built or row-built agent).
    ollama_calls: Arc<AtomicUsize>,
}

impl Fixture {
    async fn new(missing: Missing) -> Self {
        Self::build(missing, false).await
    }

    /// Same, with the `EventsService` installed as in production, so `Execute::run` takes
    /// the `agent.run` waterfall branch.
    async fn with_events(missing: Missing) -> Self {
        Self::build(missing, true).await
    }

    async fn build(missing: Missing, events: bool) -> Self {
        let pool = ares_test_support::pool().await;
        let (ollama_url, ollama_calls) = spawn_counting_ollama().await;
        let llm_calls = Arc::new(AtomicUsize::new(0));

        let root = Context::new_root();
        if events {
            root.provide(cordis::EventsService::new());
        }
        root.provide(Llm::from_client(Arc::new(CountingLlm {
            calls: Arc::clone(&llm_calls),
        })));
        if missing != Missing::TenantDb {
            root.provide(TenantDb::new(Arc::new(PostgresClient {
                pool: pool.clone(),
            })));
        }
        if missing != Missing::FleetSecrets {
            root.provide(FleetSecrets::new());
        }
        let mut exec = Execute::new();
        if missing != Missing::Registry {
            exec = exec.with_agent_registry(Arc::new(registry_with_system_agent(&ollama_url)));
        }

        let (ctx, tenant) = if missing == Missing::TenantId {
            (root, String::new())
        } else {
            let tenant = format!("tenant-{}", uuid::Uuid::new_v4());
            let tc = TenantContext::new(tenant.clone(), TenantTier::Pro);
            (request_tenant_ctx(&root, tc), tenant)
        };

        // Let a system agent (registry-built, tenant scope = `tenant`) be runnable, so that
        // a fall-through would really run it instead of failing on the model allowlist.
        TenantAllowlistStore::new(&pool)
            .allow_model(&tenant, "mock-model")
            .await
            .expect("allow mock model for the scratch tenant");

        Self {
            exec,
            ctx,
            pool,
            tenant,
            llm_calls,
            ollama_calls,
        }
    }

    async fn insert_tenant_row(&self, system_prompt: &str) {
        create_tenant_agent(
            &self.pool,
            &self.tenant,
            CreateTenantAgentRequest {
                agent_name: AGENT.to_string(),
                display_name: "product display".to_string(),
                description: None,
                config: json!({
                    "model": "default",
                    "system_prompt": system_prompt,
                    "tools": [],
                    "max_tool_iterations": 5,
                    "parallel_tools": false
                }),
            },
        )
        .await
        .expect("insert tenant agent row");
    }

    fn calls(&self) -> String {
        format!(
            "llm_calls={} ollama_calls={}",
            self.llm_calls.load(Ordering::SeqCst),
            self.ollama_calls.load(Ordering::SeqCst)
        )
    }

    /// No model call of any kind was made: neither the generic fall-through nor a
    /// registry-built system agent nor a row-built agent ran.
    fn assert_no_agent_ran(&self) {
        assert_eq!(
            self.llm_calls.load(Ordering::SeqCst),
            0,
            "the generic fall-through made a model call ({})",
            self.calls()
        );
        assert_eq!(
            self.ollama_calls.load(Ordering::SeqCst),
            0,
            "a registry-built agent called the model ({})",
            self.calls()
        );
    }

    /// Run path: the request must be refused. Returns the error.
    async fn run_refused(&self, req: &AgentRequest) -> AppError {
        match self.exec.run(req, &self.ctx).await {
            Err(e) => {
                self.assert_no_agent_ran();
                e
            }
            Ok(r) => panic!(
                "a require_tenant_agent request must be refused, but another agent ran: \
                 source={:?} agent={} content={:?} ({})",
                r.source,
                r.agent_name,
                r.response.content,
                self.calls()
            ),
        }
    }

    /// Stream path: the request must be refused before any stream is handed out.
    async fn stream_refused(&self, req: &AgentRequest) -> AppError {
        match self.exec.run_stream(req, &self.ctx).await {
            Err(e) => {
                self.assert_no_agent_ran();
                e
            }
            Ok(mut stream) => {
                let mut chunks = Vec::new();
                while let Some(item) = stream.next().await {
                    chunks.push(item);
                }
                panic!(
                    "a require_tenant_agent stream request must be refused, but another agent \
                     streamed: chunks={:?} ({})",
                    chunks,
                    self.calls()
                )
            }
        }
    }
}

fn request(require_tenant_agent: bool) -> AgentRequest {
    AgentRequest {
        agent_name: AGENT.to_string(),
        message: "hello".to_string(),
        require_tenant_agent,
        ..Default::default()
    }
}

/// The typed error for "the tenant tier could not be resolved because the runtime lacks a
/// part": `AppError::Unavailable` (HTTP 503), the same variant family as `strict_fallbacks`.
fn assert_tier_unavailable(err: &AppError) {
    assert!(
        matches!(err, AppError::Unavailable(m) if m.contains("require_tenant_agent")),
        "expected AppError::Unavailable naming require_tenant_agent, got {err:?}"
    );
}

/// The typed error for "no tenant identity on the context": `AppError::Auth` with the
/// message the deleted `resolve_required_tenant_agent_from_ctx` used for the same case.
fn assert_missing_tenant(err: &AppError) {
    assert!(
        matches!(err, AppError::Auth(m) if m == "Missing tenant context"),
        "expected AppError::Auth(\"Missing tenant context\"), got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Run path.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn required_tenant_agent_refuses_without_registry() {
    let f = Fixture::new(Missing::Registry).await;
    let err = f.run_refused(&request(true)).await;
    assert_tier_unavailable(&err);
}

#[tokio::test]
async fn required_tenant_agent_refuses_without_tenant_db() {
    let f = Fixture::new(Missing::TenantDb).await;
    let err = f.run_refused(&request(true)).await;
    assert_tier_unavailable(&err);
}

#[tokio::test]
async fn required_tenant_agent_refuses_without_fleet_secrets() {
    let f = Fixture::new(Missing::FleetSecrets).await;
    let err = f.run_refused(&request(true)).await;
    assert_tier_unavailable(&err);
}

#[tokio::test]
async fn required_tenant_agent_refuses_with_empty_user_id() {
    let f = Fixture::new(Missing::TenantId).await;
    let err = f.run_refused(&request(true)).await;
    assert_missing_tenant(&err);
}

/// Production installs the event bus, so `Execute::run` goes through the `agent.run`
/// waterfall, which round-trips the error through a slot. The typed variant must survive.
#[tokio::test]
async fn required_tenant_agent_refusal_keeps_its_variant_through_the_run_waterfall() {
    let f = Fixture::with_events(Missing::Registry).await;
    let err = f.run_refused(&request(true)).await;
    assert_tier_unavailable(&err);

    let f = Fixture::with_events(Missing::TenantId).await;
    let err = f.run_refused(&request(true)).await;
    assert_missing_tenant(&err);
}

// ---------------------------------------------------------------------------
// Stream path.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn required_tenant_agent_stream_refuses_without_registry() {
    let f = Fixture::new(Missing::Registry).await;
    let err = f.stream_refused(&request(true)).await;
    assert_tier_unavailable(&err);
}

#[tokio::test]
async fn required_tenant_agent_stream_refuses_without_tenant_db() {
    let f = Fixture::new(Missing::TenantDb).await;
    let err = f.stream_refused(&request(true)).await;
    assert_tier_unavailable(&err);
}

#[tokio::test]
async fn required_tenant_agent_stream_refuses_without_fleet_secrets() {
    let f = Fixture::new(Missing::FleetSecrets).await;
    let err = f.stream_refused(&request(true)).await;
    assert_tier_unavailable(&err);
}

#[tokio::test]
async fn required_tenant_agent_stream_refuses_with_empty_user_id() {
    let f = Fixture::new(Missing::TenantId).await;
    let err = f.stream_refused(&request(true)).await;
    assert_missing_tenant(&err);
}

// ---------------------------------------------------------------------------
// The non-required path is unchanged.
// ---------------------------------------------------------------------------

/// The same four setups with `require_tenant_agent: false` still run an agent, as today:
/// the generic fall-through for the three missing parts, the resolver's system agent for
/// the empty tenant id.
#[tokio::test]
async fn unrequired_request_still_falls_back() {
    for missing in [Missing::Registry, Missing::TenantDb, Missing::FleetSecrets] {
        let f = Fixture::new(missing).await;
        let result = f
            .exec
            .run(&request(false), &f.ctx)
            .await
            .unwrap_or_else(|e| panic!("{missing:?}: an unrequired request must still run: {e:?}"));
        assert_eq!(result.source, AgentSource::System, "{missing:?}");
        assert_eq!(result.response.content, FALLTHROUGH_ANSWER, "{missing:?}");
        assert!(
            f.llm_calls.load(Ordering::SeqCst) > 0,
            "{missing:?}: {}",
            f.calls()
        );
        assert_eq!(f.ollama_calls.load(Ordering::SeqCst), 0, "{missing:?}");
    }

    let f = Fixture::new(Missing::TenantId).await;
    let result = f
        .exec
        .run(&request(false), &f.ctx)
        .await
        .unwrap_or_else(|e| panic!("empty tenant id: an unrequired request must still run: {e:?}"));
    assert_eq!(result.source, AgentSource::System);
    assert_eq!(
        result.response.content,
        format!("SYSTEM_PROMPT={SYSTEM_PROMPT}")
    );
    assert!(f.ollama_calls.load(Ordering::SeqCst) > 0, "{}", f.calls());
}

#[tokio::test]
async fn unrequired_stream_request_still_falls_back() {
    for missing in [Missing::Registry, Missing::TenantDb, Missing::FleetSecrets] {
        let f = Fixture::new(missing).await;
        let mut stream = f
            .exec
            .run_stream(&request(false), &f.ctx)
            .await
            .unwrap_or_else(|e| panic!("{missing:?}: an unrequired stream must still run: {e:?}"));
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            text.push_str(&chunk.unwrap_or_else(|e| panic!("{missing:?}: chunk error {e:?}")));
        }
        assert_eq!(text, FALLTHROUGH_ANSWER, "{missing:?}");
        assert!(
            f.llm_calls.load(Ordering::SeqCst) > 0,
            "{missing:?}: {}",
            f.calls()
        );
    }
}

// ---------------------------------------------------------------------------
// The present-row and missing-row behaviour of the required tier is unchanged.
// ---------------------------------------------------------------------------

/// A present, enabled row still wins over the same-named system agent and is what runs.
#[tokio::test]
async fn required_request_with_present_row_runs_the_tenant_agent() {
    let f = Fixture::new(Missing::Nothing).await;
    f.insert_tenant_row("tenant-row-prompt").await;

    let result = f
        .exec
        .run(&request(true), &f.ctx)
        .await
        .expect("a present tenant row must run");
    assert_eq!(result.source, AgentSource::Tenant);
    assert_eq!(result.agent_name, AGENT);
    assert_eq!(result.response.content, "SYSTEM_PROMPT=tenant-row-prompt");
    assert!(f.ollama_calls.load(Ordering::SeqCst) > 0, "{}", f.calls());
    assert_eq!(f.llm_calls.load(Ordering::SeqCst), 0, "{}", f.calls());
}

/// A missing row is a typed not-found and the same-named system agent does not run.
#[tokio::test]
async fn required_request_without_row_is_not_found_and_runs_nothing() {
    let f = Fixture::new(Missing::Nothing).await;
    let err = f.run_refused(&request(true)).await;
    assert!(
        matches!(&err, AppError::NotFound(m) if m.contains("not found for tenant")),
        "expected AppError::NotFound, got {err:?}"
    );

    let err = f.stream_refused(&request(true)).await;
    assert!(
        matches!(&err, AppError::NotFound(m) if m.contains("not found for tenant")),
        "expected AppError::NotFound on the stream path, got {err:?}"
    );
}
