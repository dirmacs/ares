//! Item 2.12b: every agent run records where it went.
//!
//! `agent_runs.resolved_endpoint` and `agent_runs.region` (migration 039) name the provider that
//! ANSWERED the run: its endpoint reduced to scheme, host, port and path (no userinfo, no query,
//! no fragment, no trailing slash) and its region where the provider has one. Both come from the
//! resolved provider, on the server; a request never supplies either (DELEGATED ruling
//! 2026-09-29 section 3.3). `region` is visibility only: it decides nothing about where a run may go.
//!
//! What is pinned here:
//! - the sanitizer (one place: `Residency::from_raw`) and what a client reports by default;
//! - the answering provider, including a fallback that answers after the primary failed, through
//!   every layer that carries the value (the direct path, the `llm.generate` event bridge and
//!   the `agent.run` event bridge);
//! - the three writer families that write a row from here: trigger, scheduler, pipeline;
//! - the paths that cannot name their provider and so record NULL: the generic `Execute`
//!   fall-through (no agent registry), a skill run (its steps resolve providers inside `Llm`),
//!   and a run that failed (nothing answered).
//!
//! Every provider here is a stub: a loopback axum server (an Ollama-shaped chat endpoint on
//! `127.0.0.1:0`), or a stub transport around a real genai client that is never called. Nothing
//! leaves the host. Run with the features this file is gated on:
//! `cargo test -p ares-agent --features postgres,scheduler,pipeline,trigger --test run_residency`.
//!
//! Needs the scratch database named by `TEST_DATABASE_URL` (a scratch database, never `ares_test`).
#![cfg(all(
    feature = "postgres",
    feature = "scheduler",
    feature = "pipeline",
    feature = "trigger"
))]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ares_agent::context_provider::{ContextProviderHandle, NoOpContextProvider};
use ares_agent::pipeline::execute_pipeline;
use ares_agent::scheduler::SchedulerService;
use ares_agent::trigger::execute_triggered_agent;
use ares_agent::{Agent, AgentConfig, AgentRegistry, ConfigurableAgent, Execute};
use ares_llm::client::Residency;
use ares_llm::{
    AdapterKind, ClientPool, GenaiProvider, LLMClient, LLMResponse, Llm, ModelConfig, ModelParams,
    Provider, ProviderConfig, ProviderRegistry,
};
use ares_store::fleet_secrets::ProviderOverride;
use ares_store::schedules::{
    CreatePipelineRequest, CreateScheduleRequest, EventTrigger, PipelineStore, ScheduleStore,
};
use ares_store::tenant_agents::{create_tenant_agent, CreateTenantAgentRequest};
use ares_store::tenant_allowlist::TenantAllowlistStore;
use ares_store::{FleetSecrets, PostgresClient, TenantDb};
use ares_tools::{Tool, Tools};
use ares_types::models::TenantTier;
use ares_types::types::{AgentContext, AppError};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Router;
use cordis::Context;
use serde_json::json;
use sqlx::PgPool;

/// The agent every writer test runs: a system agent in the registry, named in the trigger, the
/// schedule and the pipeline.
const AGENT: &str = "residency-probe-212b";
const PRIMARY_PROVIDER: &str = "stub-primary-212b";
const FALLBACK_PROVIDER: &str = "stub-fallback-212b";
const PRIMARY_MODEL: &str = "stub-model-primary-212b";
const FALLBACK_MODEL: &str = "stub-model-fallback-212b";
/// What the agent's config names: an alias the registry resolves to the primary provider.
const MODEL_ALIAS: &str = "alias-212b";
/// A string that must never reach a column: userinfo, a query value and a fragment all carry it.
const SECRET: &str = "SECRET";

// ---------------------------------------------------------------------------
// The sanitizer and the client defaults (no database)
// ---------------------------------------------------------------------------

fn genai(kind: AdapterKind, endpoint: Option<&str>, region: Option<&str>) -> GenaiProvider {
    GenaiProvider {
        kind,
        api_key: Some("dummy-key-212b".to_string()),
        endpoint: endpoint.map(str::to_string),
        model: "stub-model-212b".to_string(),
        params: ModelParams::default(),
        headers: HashMap::new(),
        region: region.map(str::to_string),
        vertex_project: None,
        vertex_location: None,
        custom_index: None,
    }
}

async fn client_of(provider: GenaiProvider) -> Box<dyn LLMClient> {
    Provider::Genai(provider)
        .create_client()
        .await
        .expect("a genai client builds without any network")
}

fn parts(residency: &Residency) -> (Option<&str>, Option<&str>) {
    (residency.resolved_endpoint(), residency.region())
}

/// The sanitizer has one entry point, `Residency::from_raw`, and keeps scheme, host, port and
/// path only. A value that does not parse as a URL becomes `unparseable`, never the raw text.
#[test]
fn the_sanitizer_keeps_scheme_host_port_and_path_only() {
    let table: &[(&str, Option<&str>)] = &[
        (
            "https://user:SECRET@host:8443/v1?api-key=SECRET#x",
            Some("https://host:8443/v1"),
        ),
        (
            "https://api.openai.com/v1/",
            Some("https://api.openai.com/v1"),
        ),
        ("http://localhost:11434/", Some("http://localhost:11434")),
        ("http://127.0.0.1:9", Some("http://127.0.0.1:9")),
        ("https://host", Some("https://host")),
        ("https://host//", Some("https://host")),
        ("HTTPS://Example.COM/V1", Some("https://example.com/V1")),
        ("https://[::1]:8080/v1", Some("https://[::1]:8080/v1")),
        ("https://host:443/v1", Some("https://host/v1")),
        ("  https://host/v1  ", Some("https://host/v1")),
        ("https://user@host/v1", Some("https://host/v1")),
        ("https://host/v1?key=SECRET", Some("https://host/v1")),
        ("https://host/v1#SECRET", Some("https://host/v1")),
        // Not a URL with a host: recorded as `unparseable`, never raw.
        ("user:SECRET@host", Some("unparseable")),
        ("not a url SECRET", Some("unparseable")),
        ("SECRET", Some("unparseable")),
        ("mailto:SECRET@example.com", Some("unparseable")),
        ("file:///etc/passwd?SECRET", Some("unparseable")),
        ("https://", Some("unparseable")),
        ("https://:SECRET@/v1", Some("unparseable")),
        // Nothing to record.
        ("", None),
        ("   ", None),
    ];
    for (raw, expected) in table {
        let got = Residency::from_raw(Some(raw), None);
        assert_eq!(
            got.resolved_endpoint(),
            *expected,
            "sanitizing {raw:?}; region must stay NULL: {:?}",
            got.region()
        );
        assert_eq!(got.region(), None);
        if let Some(recorded) = got.resolved_endpoint() {
            for forbidden in [SECRET, "?", "#", "@"] {
                assert!(
                    !recorded.contains(forbidden),
                    "{raw:?} was recorded as {recorded:?}, which contains {forbidden:?}"
                );
            }
        }
    }
    assert_eq!(parts(&Residency::from_raw(None, None)), (None, None));
    assert_eq!(parts(&Residency::none()), (None, None));
}

/// A region is a short name; anything else is recorded as `unparseable`, never raw.
#[test]
fn the_region_sanitizer_keeps_names_and_refuses_anything_else() {
    let long = "a".repeat(65);
    let table: &[(&str, Option<&str>)] = &[
        ("ap-south-1", Some("ap-south-1")),
        ("  eu-west-1  ", Some("eu-west-1")),
        ("us-gov-west-1", Some("us-gov-west-1")),
        ("global", Some("global")),
        ("", None),
        ("   ", None),
        ("ap south 1", Some("unparseable")),
        ("key=SECRET", Some("unparseable")),
        ("https://host/SECRET", Some("unparseable")),
        (long.as_str(), Some("unparseable")),
    ];
    for (raw, expected) in table {
        let got = Residency::from_raw(None, Some(raw));
        assert_eq!(got.region(), *expected, "sanitizing the region {raw:?}");
        assert_eq!(got.resolved_endpoint(), None);
    }
}

/// `default_endpoint_is_recorded_not_null`: a provider with no endpoint override records the
/// adapter's default base URL (the one the call really goes to), not NULL.
#[tokio::test]
async fn default_endpoint_is_recorded_not_null() {
    let cases = [
        (AdapterKind::OpenAI, "https://api.openai.com/v1"),
        (AdapterKind::Anthropic, "https://api.anthropic.com/v1"),
        (
            AdapterKind::Gemini,
            "https://generativelanguage.googleapis.com/v1beta",
        ),
        (AdapterKind::Groq, "https://api.groq.com/openai/v1"),
        (AdapterKind::OpenRouter, "https://openrouter.ai/api/v1"),
        (AdapterKind::Ollama, "http://localhost:11434"),
    ];
    for (kind, expected) in cases {
        let client = client_of(genai(kind, None, None)).await;
        let residency = client.residency();
        assert_eq!(
            residency.resolved_endpoint(),
            Some(expected),
            "{kind:?} with no override"
        );
        assert_eq!(residency.region(), None, "{kind:?} has no region");
    }

    // An empty override is no override: the call goes to the default, and so does the row.
    let client = client_of(genai(AdapterKind::OpenAI, Some("  "), None)).await;
    assert_eq!(
        client.residency().resolved_endpoint(),
        Some("https://api.openai.com/v1")
    );

    // An override wins, and is sanitized.
    let client = client_of(genai(
        AdapterKind::OpenAI,
        Some("https://user:SECRET@proxy.example.test:8443/openai/v1/?k=SECRET#f"),
        None,
    ))
    .await;
    assert_eq!(
        client.residency().resolved_endpoint(),
        Some("https://proxy.example.test:8443/openai/v1")
    );
}

/// `region_is_null_when_the_provider_has_none`: a provider with no region records no region.
#[tokio::test]
async fn region_is_null_when_the_provider_has_none() {
    for kind in [
        AdapterKind::OpenAI,
        AdapterKind::Anthropic,
        AdapterKind::Gemini,
        AdapterKind::Ollama,
        AdapterKind::Groq,
    ] {
        let client = client_of(genai(kind, None, None)).await;
        assert_eq!(client.residency().region(), None, "{kind:?}");
    }
    // A stub client that knows nothing about where it goes reports nothing.
    struct Bare;
    #[async_trait::async_trait]
    impl LLMClient for Bare {
        fn model_name(&self) -> &str {
            "bare"
        }
        async fn generate(&self, _: &str) -> ares_types::types::Result<String> {
            Ok(String::new())
        }
        async fn generate_with_system(
            &self,
            _: &str,
            _: &str,
        ) -> ares_types::types::Result<String> {
            Ok(String::new())
        }
        async fn generate_with_history(
            &self,
            _: &[(String, String)],
        ) -> ares_types::types::Result<LLMResponse> {
            Err(AppError::FeatureDisabled("bare".into()))
        }
        async fn generate_with_tools(
            &self,
            _: &str,
            _: &[ares_types::types::ToolDefinition],
        ) -> ares_types::types::Result<LLMResponse> {
            Err(AppError::FeatureDisabled("bare".into()))
        }
        async fn generate_with_tools_and_history(
            &self,
            _: &[ares_llm::coordinator::ConversationMessage],
            _: &[ares_types::types::ToolDefinition],
        ) -> ares_types::types::Result<LLMResponse> {
            Err(AppError::FeatureDisabled("bare".into()))
        }
        async fn stream(&self, _: &str) -> ares_types::types::Result<StringStream> {
            Err(AppError::FeatureDisabled("bare".into()))
        }
        async fn stream_with_system(
            &self,
            _: &str,
            _: &str,
        ) -> ares_types::types::Result<StringStream> {
            Err(AppError::FeatureDisabled("bare".into()))
        }
        async fn stream_with_history(
            &self,
            _: &[(String, String)],
        ) -> ares_types::types::Result<StringStream> {
            Err(AppError::FeatureDisabled("bare".into()))
        }
    }
    assert_eq!(parts(&Bare.residency()), (None, None));
}

/// Vertex has a region too: its configured location (`global` when it is the global endpoint).
#[tokio::test]
async fn vertex_region_is_the_configured_location() {
    let mut regional = genai(AdapterKind::Vertex, None, None);
    regional.vertex_project = Some("proj-212b".to_string());
    regional.vertex_location = Some("asia-south1".to_string());
    assert_eq!(
        parts(&client_of(regional).await.residency()),
        (
            Some("https://asia-south1-aiplatform.googleapis.com/v1/projects/proj-212b/locations/asia-south1"),
            Some("asia-south1")
        )
    );

    let mut global = genai(AdapterKind::Vertex, None, None);
    global.vertex_project = Some("proj-212b".to_string());
    global.vertex_location = Some("global".to_string());
    assert_eq!(
        parts(&client_of(global).await.residency()),
        (
            Some("https://aiplatform.googleapis.com/v1/projects/proj-212b/locations/global"),
            Some("global")
        )
    );
}

/// `bedrock_region_comes_from_server_config`: the region of a Bedrock provider is read from the
/// environment variable the server's provider config names (`region_env`), and the endpoint is
/// the Bedrock runtime host of that region. Nothing a request carries can reach it.
#[tokio::test]
async fn bedrock_region_comes_from_server_config() {
    // Names private to this test binary, so no other test reads or races them. Dummy values.
    const KEY_ENV: &str = "ARES_2_12B_TEST_BEDROCK_KEY";
    const REGION_ENV: &str = "ARES_2_12B_TEST_BEDROCK_REGION";
    std::env::set_var(KEY_ENV, "dummy-bedrock-token-212b");
    std::env::set_var(REGION_ENV, "ap-south-1");

    let mut registry = ProviderRegistry::new();
    registry.register_provider(
        "bedrock-212b",
        ProviderConfig::Bedrock {
            api_key_env: KEY_ENV.to_string(),
            region_env: REGION_ENV.to_string(),
            default_model: "stub-bedrock-model-212b".to_string(),
        },
    );
    registry.register_model(
        "bedrock-alias-212b",
        ModelConfig {
            provider: "bedrock-212b".to_string(),
            model: "stub-bedrock-model-212b".to_string(),
            temperature: 0.0,
            max_tokens: 16,
        },
    );

    // Resolving the client makes no network call; asking where it goes makes none either.
    let client = registry
        .create_client_for_model("bedrock-alias-212b")
        .await
        .expect("the bedrock client builds from the server config");
    let residency = client.residency();
    assert_eq!(
        parts(&residency),
        (
            Some("https://bedrock-runtime.ap-south-1.amazonaws.com"),
            Some("ap-south-1")
        ),
        "the region comes from the configured variable"
    );

    // And a run answered by that client names it: the agent reports the pair on its metadata.
    let stub = StubTransport::new(client, Behaviour::Answer);
    let agent = ConfigurableAgent::new_with_provider_and_tool_service(
        "probe-212b",
        &agent_config(),
        Box::new(stub),
        None,
        "bedrock-212b".to_string(),
    );
    let response = agent
        .execute("hello", &agent_context())
        .await
        .expect("the stub answers");
    let metadata = response.metadata.expect("a run that answered has metadata");
    assert_eq!(metadata.provider_name, "bedrock-212b");
    assert_eq!(
        parts(&metadata.residency),
        (
            Some("https://bedrock-runtime.ap-south-1.amazonaws.com"),
            Some("ap-south-1")
        )
    );

    // A second server config, a second region: the row follows the config, nothing else.
    std::env::set_var(REGION_ENV, "eu-central-1");
    let client = registry
        .create_client_for_model("bedrock-alias-212b")
        .await
        .expect("the client builds again");
    assert_eq!(
        parts(&client.residency()),
        (
            Some("https://bedrock-runtime.eu-central-1.amazonaws.com"),
            Some("eu-central-1")
        )
    );
}

// ---------------------------------------------------------------------------
// A stub transport: a real genai client for the residency, canned answers for the calls
// ---------------------------------------------------------------------------

type StringStream =
    Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    Answer,
    Fail,
}

/// Wraps a REAL client so `residency()` is the real thing, and answers (or fails) every call
/// itself. The real client is never called, so nothing touches a network.
struct StubTransport {
    real: Box<dyn LLMClient>,
    behaviour: Behaviour,
    calls: Arc<AtomicUsize>,
}

impl StubTransport {
    fn new(real: Box<dyn LLMClient>, behaviour: Behaviour) -> Self {
        Self {
            real,
            behaviour,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn answer(&self) -> ares_types::types::Result<LLMResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.behaviour {
            Behaviour::Answer => Ok(LLMResponse {
                content: "stub answer".to_string(),
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
                usage: None,
                reasoning_content: None,
                response_id: None,
            }),
            Behaviour::Fail => Err(AppError::LLM("stub transport is down".to_string())),
        }
    }
}

#[async_trait::async_trait]
impl LLMClient for StubTransport {
    fn model_name(&self) -> &str {
        self.real.model_name()
    }
    fn residency(&self) -> Residency {
        self.real.residency()
    }
    async fn generate(&self, _: &str) -> ares_types::types::Result<String> {
        Ok(self.answer()?.content)
    }
    async fn generate_with_system(&self, _: &str, _: &str) -> ares_types::types::Result<String> {
        Ok(self.answer()?.content)
    }
    async fn generate_with_history(
        &self,
        _: &[(String, String)],
    ) -> ares_types::types::Result<LLMResponse> {
        self.answer()
    }
    async fn generate_with_tools(
        &self,
        _: &str,
        _: &[ares_types::types::ToolDefinition],
    ) -> ares_types::types::Result<LLMResponse> {
        self.answer()
    }
    async fn generate_with_tools_and_history(
        &self,
        _: &[ares_llm::coordinator::ConversationMessage],
        _: &[ares_types::types::ToolDefinition],
    ) -> ares_types::types::Result<LLMResponse> {
        self.answer()
    }
    async fn stream(&self, _: &str) -> ares_types::types::Result<StringStream> {
        Err(AppError::FeatureDisabled("stub transport".into()))
    }
    async fn stream_with_system(
        &self,
        _: &str,
        _: &str,
    ) -> ares_types::types::Result<StringStream> {
        Err(AppError::FeatureDisabled("stub transport".into()))
    }
    async fn stream_with_history(
        &self,
        _: &[(String, String)],
    ) -> ares_types::types::Result<StringStream> {
        Err(AppError::FeatureDisabled("stub transport".into()))
    }
}

fn agent_config() -> AgentConfig {
    AgentConfig {
        model: MODEL_ALIAS.to_string(),
        system_prompt: Some("residency probe".to_string()),
        tools: vec![],
        max_tool_iterations: 3,
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
    }
}

fn agent_context() -> AgentContext {
    AgentContext {
        user_id: "tenant-212b".to_string(),
        session_id: "session-212b".to_string(),
        conversation_history: vec![],
        user_memory: None,
    }
}

/// Which of the agent's two generate paths a stub run takes: with no tools it takes the plain
/// history path; with a `Tools` capability it takes the tool-calling path.
#[derive(Clone, Copy, Debug)]
enum Path {
    Plain,
    WithTools,
}

/// A run where the primary fails and a fallback (an endpoint with a region) answers.
async fn run_with_failing_primary(path: Path, event_bus: bool) -> ares_agent::AgentResponse {
    let primary = client_of(genai(
        AdapterKind::OpenAI,
        Some("https://primary.example.test/v1"),
        None,
    ))
    .await;
    let fallback = client_of(genai(
        AdapterKind::BedrockApi,
        Some("https://user:SECRET@fallback.example.test:8443/api/?k=SECRET#f"),
        Some("eu-west-1"),
    ))
    .await;
    let tools = match path {
        Path::Plain => None,
        Path::WithTools => Some(Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new()))),
    };
    let mut agent = ConfigurableAgent::new_with_provider_and_tool_service(
        "probe-212b",
        &agent_config(),
        Box::new(StubTransport::new(primary, Behaviour::Fail)),
        tools,
        "primary-provider".to_string(),
    );
    agent.set_fallback_llms_with_providers(vec![(
        "fallback-provider".to_string(),
        Box::new(StubTransport::new(fallback, Behaviour::Answer)) as Box<dyn LLMClient>,
    )]);
    if event_bus {
        let ctx = Context::new_root();
        ctx.provide(cordis::EventsService::new());
        agent.bind_request_ctx(ctx);
    }
    agent
        .execute("hello", &agent_context())
        .await
        .expect("the fallback answers")
}

/// The agent-level half of `fallback_answer_records_the_fallbacks_endpoint`: the metadata that
/// every writer reads names the provider that answered, on both generate paths and through the
/// event bridge, with the fallback's endpoint (sanitized) and region.
#[tokio::test(flavor = "multi_thread")]
async fn fallback_answer_names_the_fallback_on_every_agent_path() {
    for path in [Path::Plain, Path::WithTools] {
        for event_bus in [false, true] {
            let response = run_with_failing_primary(path, event_bus).await;
            let metadata = response.metadata.expect("a run that answered has metadata");
            let label = format!("{path:?}, event bus {event_bus}");
            assert_eq!(metadata.provider_name, "fallback-provider", "{label}");
            assert_eq!(
                parts(&metadata.residency),
                (
                    Some("https://fallback.example.test:8443/api"),
                    Some("eu-west-1")
                ),
                "{label}: the row must name the provider that answered, not the first one tried"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The loopback stub provider
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Mode {
    /// Answers every chat call (Ollama's native `/api/chat` shape).
    Answer,
    /// Fails every call with HTTP 500.
    Fail,
}

/// Serve the stub on an ephemeral loopback port. Returns its base URL and a counter of the
/// requests it received, whatever the path.
async fn spawn_stub(mode: Mode) -> (String, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let app = Router::new().fallback(move || {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            match mode {
                Mode::Answer => axum::Json(json!({
                    "model": "stub",
                    "created_at": "2026-10-01T00:00:00Z",
                    "message": {"role": "assistant", "content": "stub answer"},
                    "done": true,
                    "total_duration": 1,
                    "load_duration": 1,
                    "prompt_eval_count": 3,
                    "prompt_eval_duration": 1,
                    "eval_count": 2,
                    "eval_duration": 1
                }))
                .into_response(),
                Mode::Fail => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "stub provider is down").into_response()
                }
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the stub provider");
    let addr = listener.local_addr().expect("stub provider address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve the stub provider");
    });
    (format!("http://{addr}"), calls)
}

// ---------------------------------------------------------------------------
// The fixture: one tenant, one system agent, the real writers
// ---------------------------------------------------------------------------

struct Opts {
    /// How the primary provider's base URL is written in the server config, given the stub's
    /// real base URL.
    primary_configured_as: fn(&str) -> String,
    primary: Mode,
    /// A fallback provider (named in the fleet secrets as the primary's fallback), if any.
    fallback: Option<Mode>,
    /// Install the event bus, as production does (`Execute::run` and the agent then bridge
    /// their results through JSON).
    event_bus: bool,
    /// Give `Execute` the agent registry. Without it, `Execute::run` takes the generic
    /// fall-through, which calls the `Llm` service on the context.
    registry: bool,
    /// An endpoint and a region in places a hostile input could reach: the system agent's
    /// config `extra` map and the trigger message (see `request_supplied_endpoint_or_region_is_ignored`).
    hostile_inputs: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            primary_configured_as: |base| base.to_string(),
            primary: Mode::Answer,
            fallback: None,
            event_bus: true,
            registry: true,
            hostile_inputs: false,
        }
    }
}

struct Fixture {
    ctx: Arc<Context>,
    pool: PgPool,
    exec: Arc<Execute>,
    tenant_id: String,
    /// The real base URL of the primary stub (what the row must name when it answers).
    primary_base: String,
    primary_calls: Arc<AtomicUsize>,
    fallback_base: Option<String>,
    fallback_calls: Option<Arc<AtomicUsize>>,
}

/// One `agent_runs` row, as the writers left it.
#[derive(Debug)]
struct RunRow {
    status: String,
    model_name: String,
    provider_name: String,
    resolved_endpoint: Option<String>,
    region: Option<String>,
    /// The whole row as JSON, to look for text that must not be there.
    whole_row: String,
}

const HOSTILE_ENDPOINT: &str = "https://evil.example.test/steal";
const HOSTILE_REGION: &str = "mars-central-9";

impl Fixture {
    async fn build(opts: Opts) -> Self {
        let pool = ares_test_support::pool().await;
        let (primary_base, primary_calls) = spawn_stub(opts.primary).await;
        let fallback = match opts.fallback {
            Some(mode) => Some(spawn_stub(mode).await),
            None => None,
        };

        let mut providers = ProviderRegistry::new();
        providers.register_provider(
            PRIMARY_PROVIDER,
            ProviderConfig::Ollama {
                api_key_env: "ARES_2_12B_UNUSED".to_string(),
                base_url: (opts.primary_configured_as)(&primary_base),
                default_model: PRIMARY_MODEL.to_string(),
            },
        );
        providers.register_model(
            MODEL_ALIAS,
            ModelConfig {
                provider: PRIMARY_PROVIDER.to_string(),
                model: PRIMARY_MODEL.to_string(),
                temperature: 0.0,
                max_tokens: 64,
            },
        );
        let fleet = FleetSecrets::new();
        if let Some((fallback_base, _)) = &fallback {
            providers.register_provider(
                FALLBACK_PROVIDER,
                ProviderConfig::Ollama {
                    api_key_env: "ARES_2_12B_UNUSED".to_string(),
                    base_url: fallback_base.clone(),
                    default_model: FALLBACK_MODEL.to_string(),
                },
            );
            fleet.store(HashMap::from([(
                PRIMARY_PROVIDER.to_string(),
                ProviderOverride {
                    fallback_providers: vec![FALLBACK_PROVIDER.to_string()],
                    ..Default::default()
                },
            )]));
        }
        let providers = Arc::new(providers);

        let mut agent_registry = AgentRegistry::new(
            providers.clone(),
            Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new())),
        );
        let mut config = agent_config();
        if opts.hostile_inputs {
            config
                .extra
                .insert("resolved_endpoint".to_string(), json!(HOSTILE_ENDPOINT));
            config
                .extra
                .insert("region".to_string(), json!(HOSTILE_REGION));
        }
        agent_registry.register(AGENT, config);
        let agent_registry = Arc::new(agent_registry);

        let root = Context::new_root();
        if opts.event_bus {
            root.provide(cordis::EventsService::new());
        }
        let tenant_db = TenantDb::new(Arc::new(PostgresClient { pool: pool.clone() }));
        let tenant = tenant_db
            .create_tenant(format!("t212b-{}", uuid::Uuid::new_v4()), TenantTier::Pro)
            .await
            .expect("create the tenant");
        root.provide(tenant_db);
        root.provide(fleet);
        root.provide(ContextProviderHandle::new(Arc::new(NoOpContextProvider)));

        let allowlist = TenantAllowlistStore::new(&pool);
        for model in [PRIMARY_MODEL, FALLBACK_MODEL] {
            allowlist
                .allow_model(&tenant.id, model)
                .await
                .expect("allow the stub model");
        }

        let mut exec = Execute::new();
        if opts.registry {
            exec = exec.with_agent_registry(agent_registry);
        } else {
            // The generic fall-through asks the `Llm` service for a client. Its transport is a
            // stub, but its `residency()` is a real client's: a naive implementation could
            // record it, and this path must still record NULL (it names no provider).
            let real = client_of(genai(AdapterKind::Ollama, Some(&primary_base), None)).await;
            root.provide(Llm::from_client(Arc::new(StubTransport::new(
                real,
                Behaviour::Answer,
            ))));
        }
        let exec = Arc::new(exec);
        root.provide_arc(exec.clone());

        // (A tenant agent's config cannot carry an endpoint or a region: the validator refuses
        // unknown keys; `ares-http/tests/run_residency_http.rs` pins that.)
        let tenant_agent_config = json!({
            "model": MODEL_ALIAS,
            "system_prompt": "residency probe",
            "tools": [],
            "max_tool_iterations": 3,
            "parallel_tools": false
        });
        create_tenant_agent(
            &pool,
            &tenant.id,
            CreateTenantAgentRequest {
                agent_name: AGENT.to_string(),
                display_name: "residency probe".to_string(),
                description: None,
                config: tenant_agent_config,
            },
        )
        .await
        .expect("create the tenant agent row");

        Self {
            ctx: root,
            pool,
            exec,
            tenant_id: tenant.id,
            primary_base,
            primary_calls,
            fallback_base: fallback.as_ref().map(|(base, _)| base.clone()),
            fallback_calls: fallback.map(|(_, calls)| calls),
        }
    }

    /// The trigger writer: `execute_triggered_agent` (no `TriggerService` on the context, so the
    /// legacy dispatch that every DI path also reaches).
    async fn run_trigger(&self, message: &str) -> Result<(), String> {
        let trigger = EventTrigger {
            id: format!("trigger-212b-{}", uuid::Uuid::new_v4()),
            tenant_id: self.tenant_id.clone(),
            name: "residency probe".to_string(),
            event_type: "webhook".to_string(),
            event_config: json!({}),
            target_agent: AGENT.to_string(),
            enabled: true,
            created_at: 0,
            updated_at: 0,
        };
        execute_triggered_agent(&trigger, message, &self.ctx).await
    }

    /// The scheduler writer: one due schedule, run by the service's due pass.
    async fn run_schedule(&self) {
        let store = ScheduleStore::new(&self.pool);
        let schedule = store
            .create_schedule(&CreateScheduleRequest {
                tenant_id: self.tenant_id.clone(),
                agent_name: AGENT.to_string(),
                cron_expression: "* * * * *".to_string(),
                timezone: "UTC".to_string(),
                enabled: true,
                grace_period_seconds: 120,
            })
            .await
            .expect("create the schedule");
        // Due now, and inside its grace window, so the due pass (not the catch-up pass) runs it.
        sqlx::query("UPDATE agent_schedules SET next_run_at = $1 WHERE id = $2")
            .bind(chrono::Utc::now().timestamp() - 5)
            .bind(&schedule.id)
            .execute(&self.pool)
            .await
            .expect("make the schedule due");
        let service = SchedulerService::new(
            Arc::new(PostgresClient {
                pool: self.pool.clone(),
            }),
            self.exec.clone(),
            1_000,
        );
        service
            .run_due_schedules_owned(&self.ctx)
            .await
            .expect("the due pass runs");
    }

    /// The pipeline writer: a pipeline from another agent into the probe, fanned out.
    async fn run_pipeline(&self) {
        PipelineStore::new(&self.pool)
            .create_pipeline(&CreatePipelineRequest {
                tenant_id: self.tenant_id.clone(),
                source_agent: "source-212b".to_string(),
                target_agent: AGENT.to_string(),
                condition: None,
                enabled: true,
            })
            .await
            .expect("create the pipeline");
        let triggered =
            execute_pipeline("source-212b", "pipeline input", &self.tenant_id, &self.ctx)
                .await
                .expect("the fan-out runs");
        assert_eq!(triggered, vec![AGENT.to_string()], "the target ran");
    }

    /// The tenant's one `agent_runs` row. The pipeline writes it in a spawned task, so poll.
    async fn row(&self) -> RunRow {
        let started = Instant::now();
        loop {
            let rows: Vec<(
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                String,
            )> = sqlx::query_as(
                "SELECT status, COALESCE(model_name, ''), COALESCE(provider_name, ''), \
                            resolved_endpoint, region, row_to_json(agent_runs)::text \
                     FROM agent_runs WHERE tenant_id = $1",
            )
            .bind(&self.tenant_id)
            .fetch_all(&self.pool)
            .await
            .expect("read the tenant's agent_runs rows");
            if let Some(row) = rows.first() {
                assert_eq!(rows.len(), 1, "one run, one row: {rows:?}");
                return RunRow {
                    status: row.0.clone(),
                    model_name: row.1.clone(),
                    provider_name: row.2.clone(),
                    resolved_endpoint: row.3.clone(),
                    region: row.4.clone(),
                    whole_row: row.5.clone(),
                };
            }
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "no agent_runs row for tenant {} within 15 s",
                self.tenant_id
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

// ---------------------------------------------------------------------------
// The trigger writer, generic behaviours
// ---------------------------------------------------------------------------

/// `run_records_the_answering_providers_endpoint_and_region`: the row names the provider that
/// answered. The stub (an Ollama-shaped provider) has an endpoint and no region, so the row has
/// the sanitized endpoint and a NULL region; a provider with a region is pinned in
/// `bedrock_region_comes_from_server_config` and in the fallback test above.
#[tokio::test(flavor = "multi_thread")]
async fn run_records_the_answering_providers_endpoint_and_region() {
    let fx = Fixture::build(Opts::default()).await;
    fx.run_trigger("hello").await.expect("the trigger runs");

    let row = fx.row().await;
    assert_eq!(row.status, "completed", "{row:?}");
    assert_eq!(row.provider_name, PRIMARY_PROVIDER, "{row:?}");
    assert_eq!(row.model_name, PRIMARY_MODEL, "{row:?}");
    assert_eq!(
        row.resolved_endpoint.as_deref(),
        Some(fx.primary_base.as_str()),
        "{row:?}"
    );
    assert_eq!(
        row.region, None,
        "an Ollama-shaped provider has no region: {row:?}"
    );
    assert!(fx.primary_calls.load(Ordering::SeqCst) >= 1);
}

/// The same run with no event bus on the context: the direct path records the same pair.
#[tokio::test(flavor = "multi_thread")]
async fn run_without_the_event_bus_records_the_same_endpoint() {
    let fx = Fixture::build(Opts {
        event_bus: false,
        ..Opts::default()
    })
    .await;
    fx.run_trigger("hello").await.expect("the trigger runs");

    let row = fx.row().await;
    assert_eq!(row.status, "completed", "{row:?}");
    assert_eq!(
        row.resolved_endpoint.as_deref(),
        Some(fx.primary_base.as_str()),
        "{row:?}"
    );
}

/// `fallback_answer_records_the_fallbacks_endpoint`: the primary fails (HTTP 500) and the
/// fallback answers; the row names the fallback's endpoint and provider, not the primary's.
#[tokio::test(flavor = "multi_thread")]
async fn fallback_answer_records_the_fallbacks_endpoint() {
    let fx = Fixture::build(Opts {
        primary: Mode::Fail,
        fallback: Some(Mode::Answer),
        ..Opts::default()
    })
    .await;
    fx.run_trigger("hello").await.expect("the fallback answers");

    let fallback_base = fx.fallback_base.clone().expect("a fallback stub");
    assert!(
        fx.primary_calls.load(Ordering::SeqCst) >= 1,
        "the primary was tried first"
    );
    assert!(
        fx.fallback_calls
            .as_ref()
            .expect("a fallback counter")
            .load(Ordering::SeqCst)
            >= 1,
        "the fallback answered"
    );

    let row = fx.row().await;
    assert_eq!(row.status, "completed", "{row:?}");
    assert_eq!(row.provider_name, FALLBACK_PROVIDER, "{row:?}");
    assert_eq!(row.model_name, FALLBACK_MODEL, "{row:?}");
    assert_eq!(
        row.resolved_endpoint.as_deref(),
        Some(fallback_base.as_str()),
        "the row must name the provider that answered: {row:?}"
    );
    assert_ne!(
        row.resolved_endpoint.as_deref(),
        Some(fx.primary_base.as_str()),
        "not the first provider tried: {row:?}"
    );
}

/// `endpoint_never_carries_userinfo_query_or_fragment`: a provider configured with
/// `http://user:SECRET@127.0.0.1:PORT/v1?api-key=SECRET#x` records exactly
/// `http://127.0.0.1:PORT/v1`, and the string SECRET appears in no column of the row.
#[tokio::test(flavor = "multi_thread")]
async fn endpoint_never_carries_userinfo_query_or_fragment() {
    let fx = Fixture::build(Opts {
        primary_configured_as: |base| {
            let host = base.trim_start_matches("http://");
            format!("http://user:{SECRET}@{host}/v1?api-key={SECRET}#x")
        },
        ..Opts::default()
    })
    .await;
    fx.run_trigger("hello").await.expect("the trigger runs");

    let row = fx.row().await;
    assert_eq!(row.status, "completed", "{row:?}");
    assert_eq!(
        row.resolved_endpoint.as_deref(),
        Some(format!("{}/v1", fx.primary_base).as_str()),
        "{row:?}"
    );
    assert!(
        !row.whole_row.contains(SECRET),
        "SECRET reached a column of the row: {}",
        row.whole_row
    );
    assert!(!row.whole_row.contains("api-key"), "{}", row.whole_row);
    assert!(!row.whole_row.contains("user:"), "{}", row.whole_row);
}

/// `request_supplied_endpoint_or_region_is_ignored` (section 3.3): neither column is ever
/// filled from a request. Here the request's own text (the trigger message) and the system
/// agent's config (its `extra` map) carry an endpoint and a region; the row shows the
/// provider's, and nothing of theirs.
#[tokio::test(flavor = "multi_thread")]
async fn request_supplied_endpoint_or_region_is_ignored() {
    let fx = Fixture::build(Opts {
        hostile_inputs: true,
        ..Opts::default()
    })
    .await;
    let message = format!(
        "{{\"resolved_endpoint\":\"{HOSTILE_ENDPOINT}\",\"region\":\"{HOSTILE_REGION}\"}}\n\
         resolved_endpoint: {HOSTILE_ENDPOINT}\nregion: {HOSTILE_REGION}\n\
         x-resolved-endpoint: {HOSTILE_ENDPOINT}\nx-region: {HOSTILE_REGION}"
    );
    fx.run_trigger(&message).await.expect("the trigger runs");

    let row = fx.row().await;
    assert_eq!(row.status, "completed", "{row:?}");
    assert_eq!(
        row.resolved_endpoint.as_deref(),
        Some(fx.primary_base.as_str()),
        "{row:?}"
    );
    assert_eq!(row.region, None, "{row:?}");
    for hostile in [HOSTILE_ENDPOINT, HOSTILE_REGION, "evil.example"] {
        assert!(
            !row.whole_row.contains(hostile),
            "{hostile} reached the row: {}",
            row.whole_row
        );
    }
}

// ---------------------------------------------------------------------------
// One test per writer family that writes a row from here
// ---------------------------------------------------------------------------

/// Trigger: the row has the answering provider's endpoint.
#[tokio::test(flavor = "multi_thread")]
async fn trigger_writer_records_the_answering_endpoint() {
    let fx = Fixture::build(Opts {
        primary: Mode::Fail,
        fallback: Some(Mode::Answer),
        ..Opts::default()
    })
    .await;
    fx.run_trigger("hello").await.expect("the trigger runs");

    let row = fx.row().await;
    assert_eq!(row.provider_name, FALLBACK_PROVIDER, "{row:?}");
    assert_eq!(
        row.resolved_endpoint.as_deref(),
        fx.fallback_base.as_deref(),
        "{row:?}"
    );
}

/// Scheduler: the row has the answering provider's endpoint.
#[tokio::test(flavor = "multi_thread")]
async fn scheduler_writer_records_the_answering_endpoint() {
    let fx = Fixture::build(Opts {
        primary: Mode::Fail,
        fallback: Some(Mode::Answer),
        ..Opts::default()
    })
    .await;
    fx.run_schedule().await;

    let row = fx.row().await;
    assert_eq!(row.status, "completed", "{row:?}");
    assert_eq!(row.provider_name, FALLBACK_PROVIDER, "{row:?}");
    assert_eq!(
        row.resolved_endpoint.as_deref(),
        fx.fallback_base.as_deref(),
        "{row:?}"
    );
    assert_eq!(row.region, None, "{row:?}");
}

/// Pipeline: the row has the answering provider's endpoint.
#[tokio::test(flavor = "multi_thread")]
async fn pipeline_writer_records_the_answering_endpoint() {
    let fx = Fixture::build(Opts {
        primary: Mode::Fail,
        fallback: Some(Mode::Answer),
        ..Opts::default()
    })
    .await;
    fx.run_pipeline().await;

    let row = fx.row().await;
    assert_eq!(row.status, "completed", "{row:?}");
    assert_eq!(row.provider_name, FALLBACK_PROVIDER, "{row:?}");
    assert_eq!(
        row.resolved_endpoint.as_deref(),
        fx.fallback_base.as_deref(),
        "{row:?}"
    );
    assert_eq!(row.region, None, "{row:?}");
}

// ---------------------------------------------------------------------------
// The paths that cannot name their provider record NULL (each pinned)
// ---------------------------------------------------------------------------

/// A run that failed: nothing answered, so both columns are NULL (the trigger writer's failed
/// close-out). The primary was reached and failed; the row must not claim it.
#[tokio::test(flavor = "multi_thread")]
async fn failed_run_records_no_endpoint() {
    let fx = Fixture::build(Opts {
        primary: Mode::Fail,
        ..Opts::default()
    })
    .await;
    let outcome = fx.run_trigger("hello").await;
    assert!(outcome.is_err(), "no provider answered: {outcome:?}");

    let row = fx.row().await;
    assert_eq!(row.status, "failed", "{row:?}");
    assert_eq!(row.resolved_endpoint, None, "{row:?}");
    assert_eq!(row.region, None, "{row:?}");
    assert!(fx.primary_calls.load(Ordering::SeqCst) >= 1);
}

/// The generic `Execute` fall-through (no agent registry on `Execute` or the context) calls the
/// `Llm` service and names no provider (its row says `unknown`), so it records NULL even though
/// the client it used could say where it goes.
#[tokio::test(flavor = "multi_thread")]
async fn generic_fallthrough_run_records_null_residency() {
    let fx = Fixture::build(Opts {
        registry: false,
        ..Opts::default()
    })
    .await;
    fx.run_trigger("hello")
        .await
        .expect("the fall-through answers");

    let row = fx.row().await;
    assert_eq!(row.status, "completed", "{row:?}");
    assert_eq!(row.provider_name, "unknown", "{row:?}");
    assert_eq!(row.resolved_endpoint, None, "{row:?}");
    assert_eq!(row.region, None, "{row:?}");
}

/// A skill run: its steps resolve providers inside `Llm`, so the run has no single provider
/// and records NULL. The skill's one step is answered by the `llm.complete` hook, so no
/// provider is called.
#[tokio::test(flavor = "multi_thread")]
async fn skill_run_records_null_residency() {
    use ares_agent::skills::SkillEngine;

    let fx = Fixture::build(Opts::default()).await;

    let mut registry = ProviderRegistry::new();
    registry.register_provider(
        "skill-local-212b",
        ProviderConfig::Ollama {
            api_key_env: "ARES_2_12B_UNUSED".to_string(),
            base_url: fx.primary_base.clone(),
            default_model: PRIMARY_MODEL.to_string(),
        },
    );
    registry.register_model(
        "skill-model-212b",
        ModelConfig {
            provider: "skill-local-212b".to_string(),
            model: PRIMARY_MODEL.to_string(),
            temperature: 0.0,
            max_tokens: 16,
        },
    );
    let llm = Arc::new(Llm::new(
        Arc::new(registry),
        Arc::new(ClientPool::with_defaults()),
        None,
    ));
    let engine = Arc::new(SkillEngine::new(
        fx.pool.clone(),
        Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new())),
        llm,
    ));
    fx.ctx.provide_arc(engine);
    let _skill_step_answers = fx
        .ctx
        .get::<cordis::EventsService>()
        .expect("the fixture installs the event bus")
        .on_waterfall("llm.complete".into(), |_payload, _next| async move {
            Ok(json!({"content": "skill step answer"}))
        });

    let skill = ares_store::skills::SkillStore::new(&fx.pool)
        .create_skill(&ares_store::skills::CreateSkillRequest {
            tenant_id: fx.tenant_id.clone(),
            name: format!("residency-skill-{}", uuid::Uuid::new_v4()),
            display_name: "residency skill".to_string(),
            description: None,
            skill_type: "workflow".to_string(),
            steps: json!([
                {"type": "llm_call", "prompt": "say hi", "model_tier": "skill-model-212b"}
            ]),
            input_schema: None,
            output_schema: None,
            tools: None,
            is_public: false,
            created_by: None,
        })
        .await
        .expect("create the skill");
    TenantAllowlistStore::new(&fx.pool)
        .allow_model(&fx.tenant_id, "skill-model-212b")
        .await
        .expect("allow the skill's model");
    sqlx::query("UPDATE tenant_agents SET config = config || $1 WHERE tenant_id = $2")
        .bind(json!({"skill_id": skill.id}))
        .bind(&fx.tenant_id)
        .execute(&fx.pool)
        .await
        .expect("point the tenant agent at the skill");

    fx.run_trigger("hello")
        .await
        .expect("the skill run succeeds");

    let row = fx.row().await;
    assert_eq!(row.provider_name, "skill", "{row:?}");
    assert_eq!(row.resolved_endpoint, None, "{row:?}");
    assert_eq!(row.region, None, "{row:?}");
}
