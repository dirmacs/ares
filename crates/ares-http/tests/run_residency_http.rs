//! Item 2.12b over HTTP: the three HTTP writers record where the run went, the admin runs list
//! shows it, and no client-facing response carries it.
//!
//! `agent_runs.resolved_endpoint` and `agent_runs.region` (migration 039) name the provider that
//! ANSWERED the run: its endpoint (scheme, host, port, path only) and its region where it has
//! one. A request never supplies either (DELEGATED ruling 2026-09-29 section 3.3).
//!
//! Every test drives the real router (`create_router`: API-key auth or a JWT, the `TenantDb`
//! injection, `track_usage`, then the handler) in process with `tower::ServiceExt::oneshot`,
//! against a stub Ollama provider on `127.0.0.1:0`. The new columns are read with raw SQL.
//!
//! The stub is an Ollama-shaped provider: it has an endpoint and no region, so the rows here have
//! the endpoint and a NULL region. A provider with a region is pinned in
//! `ares-agent/tests/run_residency.rs` (no provider with a region can answer from a loopback stub).
//!
//! Requires a live Postgres named by `TEST_DATABASE_URL` (a scratch database, never `ares_test`).
//! Configured and unreachable, a test panics (naming the variable only); unconfigured, it skips
//! with the crate's convention (`tests/common/mod.rs`).

#![cfg(feature = "postgres")]

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ares_agent::{AgentConfig, AgentRegistry};
use ares_http::active_runs::ActiveRuns;
use ares_http::auth::jwt::AuthService;
use ares_http::config::{AuthConfig, ServerConfig};
use ares_http::overlay::{
    AresConfig, AresConfigManager, BillingConfig, DatabaseConfig, DynamicConfigPaths, RagConfig,
};
use ares_llm::{ModelConfig, ProviderConfig, ProviderRegistry};
use ares_store::fleet_secrets::ProviderOverride;
use ares_store::tenant_agents::{create_tenant_agent, CreateTenantAgentRequest};
use ares_store::{FleetSecrets, TenantDb};
use ares_tools::Tools;
use ares_types::models::TenantTier;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::Router;
use cordis::Context;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

/// The tenant agent the v1 run route runs.
const AGENT: &str = "residency-probe-212b";
/// The system agent the chat routes run (`agent_type: product`).
const CHAT_AGENT: &str = "product";
const PRIMARY_PROVIDER: &str = "stub-primary-212b";
const FALLBACK_PROVIDER: &str = "stub-fallback-212b";
const PRIMARY_MODEL: &str = "stub-model-primary-212b";
const FALLBACK_MODEL: &str = "stub-model-fallback-212b";
const MODEL_ALIAS: &str = "alias-212b";
const JWT_SECRET: &str = "test-run-residency-http-jwt-secret-not-real";
const ADMIN_SECRET: &str = "test-run-residency-http-admin-secret-not-real";

const HOSTILE_ENDPOINT: &str = "https://evil.example.test/steal";
const HOSTILE_REGION: &str = "mars-central-9";

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

// ---------------------------------------------------------------------------
// The stub provider
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Stub {
    /// Answers every chat call (Ollama's native `/api/chat` shape).
    Answer,
    /// Fails every call with HTTP 500.
    Fail,
}

/// Serve the stub on an ephemeral loopback port. Returns its base URL and a counter of the
/// requests it received.
async fn spawn_stub(mode: Stub) -> (String, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let app = Router::new().fallback(move || {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            match mode {
                Stub::Answer => axum::Json(json!({
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
                Stub::Fail => {
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
// The fixture: one tenant, a tenant agent, a system agent, the real router
// ---------------------------------------------------------------------------

/// `AresConfig` for the admin list handler (it reads the billing config).
fn minimal_config() -> AresConfig {
    AresConfig {
        server: ServerConfig::default(),
        auth: AuthConfig {
            jwt_secret_env: "JWT_SECRET".into(),
            jwt_access_expiry: 900,
            jwt_refresh_expiry: 604_800,
            api_key_env: "API_KEY".into(),
        },
        database: DatabaseConfig::default(),
        nvidia: None,
        config: DynamicConfigPaths::default(),
        providers: HashMap::new(),
        models: HashMap::new(),
        tools: HashMap::new(),
        agents: HashMap::new(),
        workflows: HashMap::new(),
        rag: RagConfig::default(),
        billing: BillingConfig::default(),
        skills: None,
    }
}

fn system_agent_config() -> AgentConfig {
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

struct Fixture {
    app: Router,
    pool: PgPool,
    tenant_id: String,
    api_key: String,
    /// The JWT the legacy chat route takes (HS256, an ARES product role, this tenant).
    jwt: String,
    /// The real base URL of the primary stub, which the row must name when it answers.
    primary_base: String,
    /// The real base URL of the fallback stub, when there is one.
    fallback_base: Option<String>,
}

/// What one request returned.
struct Reply {
    status: StatusCode,
    headers: Vec<(String, String)>,
    text: String,
    body: Value,
}

/// `status, provider_name, resolved_endpoint, region` and the whole row as JSON, as read.
type ResidencyColumns = (String, String, Option<String>, Option<String>, String);

/// The run's `agent_runs` row, as far as residency goes.
#[derive(Debug)]
struct ResidencyRow {
    status: String,
    provider_name: String,
    resolved_endpoint: Option<String>,
    region: Option<String>,
    whole_row: String,
}

impl Fixture {
    /// `None` only for the unconfigured skip. `primary`: how the primary stub behaves.
    /// `fallback`: a fallback stub (named in the fleet secrets), if any.
    async fn new(primary: Stub, fallback: Option<Stub>) -> Option<Self> {
        common::live_db_url(&common::current_test_name()).await?;
        // The admin routes authenticate on this variable; a dummy value, set to the same thing
        // by every test of this binary.
        std::env::set_var("ADMIN_API_KEY", ADMIN_SECRET);

        let pg = Arc::new(ares_test_support::client().await);
        let pool = pg.pool.clone();
        let tenant_db = Arc::new(TenantDb::new(pg.clone()));

        let (primary_base, _) = spawn_stub(primary).await;
        let fallback_stub = match fallback {
            Some(mode) => Some(spawn_stub(mode).await),
            None => None,
        };

        let mut providers = HashMap::new();
        providers.insert(
            PRIMARY_PROVIDER.to_string(),
            ProviderConfig::Ollama {
                api_key_env: "ARES_2_12B_UNUSED".to_string(),
                base_url: primary_base.clone(),
                default_model: PRIMARY_MODEL.to_string(),
            },
        );
        let fleet = FleetSecrets::new();
        if let Some((fallback_base, _)) = &fallback_stub {
            providers.insert(
                FALLBACK_PROVIDER.to_string(),
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
        let mut models = HashMap::new();
        models.insert(
            MODEL_ALIAS.to_string(),
            ModelConfig {
                provider: PRIMARY_PROVIDER.to_string(),
                model: PRIMARY_MODEL.to_string(),
                temperature: 0.0,
                max_tokens: 64,
            },
        );
        let provider_registry = Arc::new(ProviderRegistry::from_config(providers, models, None));
        let tools = Arc::new(Tools::from_static([]));
        let agent_registry = Arc::new(AgentRegistry::from_config(
            HashMap::from([(CHAT_AGENT.to_string(), system_agent_config())]),
            provider_registry.clone(),
            tools.clone(),
        ));

        let ctx = Context::new_root();
        ctx.provide_arc(pg);
        ctx.provide_arc(tenant_db.clone());
        ctx.provide_arc(provider_registry);
        ctx.provide_arc(agent_registry);
        ctx.provide_arc(tools);
        ctx.provide_arc(Arc::new(AresConfigManager::from_config(minimal_config())));
        ctx.provide(ares_agent::EmergencyStop::new(false));
        ctx.provide(ares_agent::ContextProviderHandle::new(Arc::new(
            ares_agent::context_provider::NoOpContextProvider,
        )));
        ctx.provide(fleet);
        ctx.provide(ActiveRuns::new());
        ctx.provide_arc(Arc::new(ares_agent::execution::Execute::new()));

        let auth = Arc::new(AuthService::new(JWT_SECRET.to_string(), 900, 604_800));
        let app = Router::new()
            .nest(
                "/api",
                ares_http::api::routes::create_router(auth, tenant_db.clone()),
            )
            .with_state(ctx);

        let tenant = tenant_db
            .create_tenant(unique("t212b"), TenantTier::Enterprise)
            .await
            .expect("create the tenant");
        let allowlist = ares_store::tenant_allowlist::TenantAllowlistStore::new(&pool);
        for model in [PRIMARY_MODEL, FALLBACK_MODEL] {
            allowlist
                .allow_model(&tenant.id, model)
                .await
                .expect("allow the stub model");
        }
        let (_, api_key) = tenant_db
            .create_api_key(&tenant.id, "t212b-key".to_string(), None, None)
            .await
            .expect("create the API key");
        create_tenant_agent(
            &pool,
            &tenant.id,
            CreateTenantAgentRequest {
                agent_name: AGENT.to_string(),
                display_name: "Residency probe".to_string(),
                description: None,
                config: json!({
                    "model": MODEL_ALIAS,
                    "system_prompt": "residency probe",
                    "tools": [],
                    "max_tool_iterations": 5,
                    "parallel_tools": false
                }),
            },
        )
        .await
        .expect("create the tenant agent");

        let now = chrono::Utc::now().timestamp();
        let jwt = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &json!({
                "sub": unique("user-212b"),
                "email": "user-212b@example.test",
                "exp": now + 3600,
                "iat": now,
                "tenant_id": tenant.id,
                "roles": { "ares": [{ "role": "user" }] },
            }),
            &jsonwebtoken::EncodingKey::from_secret(JWT_SECRET.as_bytes()),
        )
        .expect("sign the test JWT");

        Some(Self {
            app,
            pool,
            tenant_id: tenant.id,
            api_key,
            jwt,
            primary_base,
            fallback_base: fallback_stub.map(|(base, _)| base),
        })
    }

    async fn send(
        &self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Reply {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string())),
            None => builder.body(Body::empty()),
        }
        .expect("build the request");
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("the router answers");
        let status = response.status();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the response");
        let text = String::from_utf8_lossy(&bytes).to_string();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply {
            status,
            headers,
            text,
            body,
        }
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.api_key)
    }

    /// One `POST /api/v1/agents/{AGENT}/run`, with extra headers and body keys.
    async fn v1_run(&self, headers: &[(&str, &str)], extra_body: Value) -> Reply {
        let mut body = json!({"message": "hello"});
        if let (Some(target), Some(extra)) = (body.as_object_mut(), extra_body.as_object()) {
            for (key, value) in extra {
                target.insert(key.clone(), value.clone());
            }
        }
        let bearer = self.bearer();
        let mut all = vec![("authorization", bearer.as_str())];
        all.extend_from_slice(headers);
        self.send(
            "POST",
            &format!("/api/v1/agents/{AGENT}/run"),
            &all,
            Some(body),
        )
        .await
    }

    async fn v1_chat(&self) -> Reply {
        let bearer = self.bearer();
        self.send(
            "POST",
            "/api/v1/chat",
            &[("authorization", bearer.as_str())],
            Some(json!({"message": "hello", "agent_type": CHAT_AGENT})),
        )
        .await
    }

    async fn legacy_chat(&self) -> Reply {
        let bearer = format!("Bearer {}", self.jwt);
        self.send(
            "POST",
            "/api/chat",
            &[("authorization", bearer.as_str())],
            Some(json!({"message": "hello", "agent_type": CHAT_AGENT})),
        )
        .await
    }

    /// The tenant's one `agent_runs` row for `agent_name` (the chat writers insert in a spawned
    /// task, so poll).
    async fn row_for_agent(&self, agent_name: &str) -> ResidencyRow {
        let started = Instant::now();
        loop {
            let rows: Vec<ResidencyColumns> = sqlx::query_as(
                "SELECT status, COALESCE(provider_name, ''), resolved_endpoint, region, \
                            row_to_json(agent_runs)::text \
                     FROM agent_runs WHERE tenant_id = $1 AND agent_name = $2",
            )
            .bind(&self.tenant_id)
            .bind(agent_name)
            .fetch_all(&self.pool)
            .await
            .expect("read the tenant's agent_runs rows");
            if let Some(row) = rows.first() {
                assert_eq!(rows.len(), 1, "one run, one row: {rows:?}");
                return ResidencyRow {
                    status: row.0.clone(),
                    provider_name: row.1.clone(),
                    resolved_endpoint: row.2.clone(),
                    region: row.3.clone(),
                    whole_row: row.4.clone(),
                };
            }
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "no agent_runs row for {agent_name} (tenant {}) within 15 s",
                self.tenant_id
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

fn assert_ok_reply(reply: &Reply) {
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
}

// ---------------------------------------------------------------------------
// The three HTTP writers write the row with both fields
// ---------------------------------------------------------------------------

/// A v1 run: the row names the stub provider's endpoint; the stub has no region, so NULL.
#[tokio::test]
async fn v1_run_writes_the_answering_endpoint_and_region() {
    let Some(fx) = Fixture::new(Stub::Answer, None).await else {
        return;
    };
    let reply = fx.v1_run(&[], json!({})).await;
    assert_ok_reply(&reply);
    assert_eq!(reply.body["status"], "completed", "{}", reply.text);

    let row = fx.row_for_agent(AGENT).await;
    assert_eq!(row.status, "completed", "{row:?}");
    assert_eq!(row.provider_name, PRIMARY_PROVIDER, "{row:?}");
    assert_eq!(
        row.resolved_endpoint.as_deref(),
        Some(fx.primary_base.as_str()),
        "{row:?}"
    );
    assert_eq!(
        row.region, None,
        "an Ollama-shaped provider has no region: {row:?}"
    );
}

/// A v1 run whose primary fails and whose fallback answers: the row names the provider that
/// answered, not the first one tried.
#[tokio::test]
async fn v1_run_answered_by_a_fallback_records_the_fallbacks_endpoint() {
    let Some(fx) = Fixture::new(Stub::Fail, Some(Stub::Answer)).await else {
        return;
    };
    let reply = fx.v1_run(&[], json!({})).await;
    assert_ok_reply(&reply);
    assert_eq!(reply.body["status"], "completed", "{}", reply.text);

    let row = fx.row_for_agent(AGENT).await;
    assert_eq!(row.provider_name, FALLBACK_PROVIDER, "{row:?}");
    assert_eq!(
        row.resolved_endpoint.as_deref(),
        fx.fallback_base.as_deref(),
        "{row:?}"
    );
    assert_ne!(
        row.resolved_endpoint.as_deref(),
        Some(fx.primary_base.as_str()),
        "{row:?}"
    );
}

/// A v1 chat: the row names the stub provider's endpoint.
#[tokio::test]
async fn v1_chat_writes_the_answering_endpoint_and_region() {
    let Some(fx) = Fixture::new(Stub::Answer, None).await else {
        return;
    };
    let reply = fx.v1_chat().await;
    assert_ok_reply(&reply);

    let row = fx.row_for_agent(CHAT_AGENT).await;
    assert_eq!(row.status, "completed", "{row:?}");
    assert_eq!(
        row.resolved_endpoint.as_deref(),
        Some(fx.primary_base.as_str()),
        "{row:?}"
    );
    assert_eq!(row.region, None, "{row:?}");
}

/// A legacy chat (`POST /api/chat`, JWT): the row names the stub provider's endpoint.
#[tokio::test]
async fn legacy_chat_writes_the_answering_endpoint_and_region() {
    let Some(fx) = Fixture::new(Stub::Answer, None).await else {
        return;
    };
    let reply = fx.legacy_chat().await;
    assert_ok_reply(&reply);

    let row = fx.row_for_agent(CHAT_AGENT).await;
    assert_eq!(row.status, "completed", "{row:?}");
    assert_eq!(
        row.resolved_endpoint.as_deref(),
        Some(fx.primary_base.as_str()),
        "{row:?}"
    );
    assert_eq!(row.region, None, "{row:?}");
}

// ---------------------------------------------------------------------------
// Where it is shown, and where it is not
// ---------------------------------------------------------------------------

/// The admin runs list returns both fields, per run: the endpoint, and a region key that is
/// present and null.
#[tokio::test]
async fn admin_runs_list_returns_resolved_endpoint_and_region() {
    let Some(fx) = Fixture::new(Stub::Answer, None).await else {
        return;
    };
    assert_ok_reply(&fx.v1_run(&[], json!({})).await);
    let _ = fx.row_for_agent(AGENT).await;

    let reply = fx
        .send(
            "GET",
            &format!("/api/admin/tenants/{}/agents/{AGENT}/runs", fx.tenant_id),
            &[("x-admin-secret", ADMIN_SECRET)],
            None,
        )
        .await;
    assert_ok_reply(&reply);
    let runs = reply.body.as_array().expect("the runs list is an array");
    assert_eq!(runs.len(), 1, "{}", reply.text);
    let run = runs[0].as_object().expect("a run is an object");
    assert_eq!(
        run.get("resolved_endpoint").and_then(Value::as_str),
        Some(fx.primary_base.as_str()),
        "{}",
        reply.text
    );
    assert!(
        run.contains_key("region") && run["region"].is_null(),
        "the region key is present and null for a provider with no region: {}",
        reply.text
    );
}

/// A v1 run's response body (and headers) do NOT contain the residency fields, and neither does
/// the client-facing run list. The row has them; the client never sees them.
#[tokio::test]
async fn v1_responses_do_not_expose_the_residency_fields() {
    let Some(fx) = Fixture::new(Stub::Answer, None).await else {
        return;
    };
    let reply = fx.v1_run(&[], json!({})).await;
    assert_ok_reply(&reply);
    let row = fx.row_for_agent(AGENT).await;
    assert_eq!(
        row.resolved_endpoint.as_deref(),
        Some(fx.primary_base.as_str()),
        "the row records it (otherwise this test proves nothing): {row:?}"
    );

    let host_and_port = fx.primary_base.trim_start_matches("http://").to_string();
    let bearer = fx.bearer();
    let list = fx
        .send(
            "GET",
            &format!("/api/v1/agents/{AGENT}/runs"),
            &[("authorization", bearer.as_str())],
            None,
        )
        .await;
    assert_ok_reply(&list);
    assert!(
        list.body["items"].as_array().is_some_and(|i| i.len() == 1),
        "{}",
        list.text
    );

    for (what, reply) in [("the run response", &reply), ("the run list", &list)] {
        for forbidden in ["resolved_endpoint", "region", host_and_port.as_str()] {
            assert!(
                !reply.text.contains(forbidden),
                "{what} contains {forbidden:?}: {}",
                reply.text
            );
        }
        for (name, value) in &reply.headers {
            assert!(
                !value.contains(host_and_port.as_str()) && !name.contains("endpoint"),
                "{what} has a header {name}: {value}"
            );
        }
    }
}

/// `request_supplied_endpoint_or_region_is_ignored` (section 3.3): a request that carries an
/// endpoint or a region (headers, body keys, query keys) changes nothing in the row. A tenant's
/// own agent config cannot carry either name at all: the config validator refuses unknown keys
/// (if either name is ever added to the allowed keys, per-agent region routing is CR-2 and this
/// test fails so the decision is made on purpose).
#[tokio::test]
async fn request_supplied_endpoint_or_region_is_ignored() {
    let Some(fx) = Fixture::new(Stub::Answer, None).await else {
        return;
    };
    for key in ["resolved_endpoint", "region"] {
        let outcome = create_tenant_agent(
            &fx.pool,
            &fx.tenant_id,
            CreateTenantAgentRequest {
                agent_name: format!("config-with-{key}"),
                display_name: "refused".to_string(),
                description: None,
                config: json!({
                    "model": MODEL_ALIAS,
                    key: HOSTILE_ENDPOINT,
                }),
            },
        )
        .await;
        let error = outcome.expect_err("a tenant agent config with this key must be refused");
        assert!(
            error
                .to_string()
                .contains("Unknown tenant agent config key"),
            "{key}: {error}"
        );
    }

    let bearer = fx.bearer();
    let reply = fx
        .send(
            "POST",
            &format!(
                "/api/v1/agents/{AGENT}/run?resolved_endpoint={HOSTILE_ENDPOINT}&region={HOSTILE_REGION}"
            ),
            &[
                ("authorization", bearer.as_str()),
                ("x-resolved-endpoint", HOSTILE_ENDPOINT),
                ("x-region", HOSTILE_REGION),
                ("x-ares-region", HOSTILE_REGION),
                ("resolved-endpoint", HOSTILE_ENDPOINT),
            ],
            Some(json!({
                "message": "hello",
                "resolved_endpoint": HOSTILE_ENDPOINT,
                "region": HOSTILE_REGION,
                "input": {"resolved_endpoint": HOSTILE_ENDPOINT, "region": HOSTILE_REGION},
                "metadata": {"resolved_endpoint": HOSTILE_ENDPOINT, "region": HOSTILE_REGION}
            })),
        )
        .await;
    assert_ok_reply(&reply);
    assert_eq!(reply.body["status"], "completed", "{}", reply.text);

    let row = fx.row_for_agent(AGENT).await;
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

    // The chat routes take a body too.
    let chat = fx
        .send(
            "POST",
            "/api/v1/chat",
            &[
                ("authorization", bearer.as_str()),
                ("x-resolved-endpoint", HOSTILE_ENDPOINT),
                ("x-region", HOSTILE_REGION),
            ],
            Some(json!({
                "message": "hello",
                "agent_type": CHAT_AGENT,
                "resolved_endpoint": HOSTILE_ENDPOINT,
                "region": HOSTILE_REGION
            })),
        )
        .await;
    assert_ok_reply(&chat);
    let chat_row = fx.row_for_agent(CHAT_AGENT).await;
    assert_eq!(
        chat_row.resolved_endpoint.as_deref(),
        Some(fx.primary_base.as_str()),
        "{chat_row:?}"
    );
    assert_eq!(chat_row.region, None, "{chat_row:?}");
    for hostile in [HOSTILE_ENDPOINT, HOSTILE_REGION, "evil.example"] {
        assert!(
            !chat_row.whole_row.contains(hostile),
            "{hostile} reached the chat row: {}",
            chat_row.whole_row
        );
    }
}
