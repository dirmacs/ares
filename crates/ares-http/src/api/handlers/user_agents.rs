use crate::HttpError;
use crate::Result;
use crate::{
    db::postgres::UserAgent, db::traits::DatabaseClient, types::AppError,
    utils::toml_config::AgentConfig,
};
use crate::auth::middleware::AuthUser;
use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    Json,
};
use cordis::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateUserAgentReq {
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub model: String,
    pub system_prompt: Option<String>,
    pub tools: Vec<String>,
    #[serde(default = "default_max_iterations")]
    pub max_tool_iterations: i32,
    #[serde(default)]
    pub parallel_tools: bool,
    #[serde(default)]
    pub is_public: bool,
    #[serde(default)]
    pub extra: HashMap<String, serde_json::Value>,
}

fn default_max_iterations() -> i32 {
    10
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UserAgentResponse {
    pub id: String,
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub model: String,
    pub system_prompt: Option<String>,
    pub tools: Vec<String>,
    pub max_tool_iterations: i32,
    pub parallel_tools: bool,
    pub is_public: bool,
    pub usage_count: i32,
    pub average_rating: Option<f32>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<UserAgent> for UserAgentResponse {
    fn from(agent: UserAgent) -> Self {
        user_agent_response_from_db(agent)
    }
}

/// Map a persisted [`UserAgent`] row into an API response (pure; no I/O).
pub(crate) fn user_agent_response_from_db(agent: UserAgent) -> UserAgentResponse {
    let tools = parse_tools_field(&agent.tools);
    let rating = agent.average_rating();
    UserAgentResponse {
        id: agent.id,
        name: agent.name,
        display_name: agent.display_name,
        description: agent.description,
        model: agent.model,
        system_prompt: agent.system_prompt,
        tools,
        max_tool_iterations: agent.max_tool_iterations,
        parallel_tools: agent.parallel_tools,
        is_public: agent.is_public,
        usage_count: agent.usage_count,
        average_rating: rating,
        created_at: agent.created_at,
        updated_at: agent.updated_at,
    }
}

/// Parse the JSON-encoded tools column into a vector (invalid JSON → empty).
pub(crate) fn parse_tools_field(tools: &str) -> Vec<String> {
    serde_json::from_str(tools).unwrap_or_default()
}

/// Parse the JSON-encoded extra column into a map (invalid JSON → empty).
pub(crate) fn parse_extra_config(extra: &str) -> HashMap<String, serde_json::Value> {
    serde_json::from_str(extra).unwrap_or_default()
}

/// Build a synthetic system [`UserAgent`] from static TOML/TOON [`AgentConfig`].
pub(crate) fn system_agent_from_config(agent_name: &str, cfg: &AgentConfig, now: i64) -> UserAgent {
    UserAgent {
        id: format!("system-{agent_name}"),
        user_id: "system".into(),
        name: agent_name.into(),
        display_name: None,
        description: None,
        model: cfg.model.clone(),
        system_prompt: cfg.system_prompt.clone(),
        tools: serde_json::to_string(&cfg.tools).unwrap_or_else(|_| "[]".into()),
        max_tool_iterations: cfg.max_tool_iterations as i32,
        parallel_tools: cfg.parallel_tools,
        extra: "{}".into(),
        is_public: true,
        usage_count: 0,
        rating_sum: 0,
        rating_count: 0,
        created_at: now,
        updated_at: now,
    }
}

/// Pure 3-tier resolution given already-fetched candidates (no I/O).
pub(crate) fn resolve_from_candidates(
    user_agent: Option<UserAgent>,
    public_agent: Option<UserAgent>,
    system_config: Option<&AgentConfig>,
    agent_name: &str,
    now: i64,
) -> Result<(UserAgent, String)> {
    if let Some(agent) = user_agent {
        return Ok((agent, "user".into()));
    }
    if let Some(agent) = public_agent {
        return Ok((agent, "community".into()));
    }
    if let Some(cfg) = system_config {
        return Ok((
            system_agent_from_config(agent_name, cfg, now),
            "system".into(),
        ));
    }
    Err(HttpError::from(AppError::NotFound(format!(
        "Agent '{agent_name}' not found"
    ))))
}

pub async fn resolve_agent(
    state: &Arc<Context>,
    user_id: &str,
    agent_name: String,
) -> Result<(UserAgent, String)> {
    let user_id = ares_agent::user_id_from_ctx(state, user_id);
    let user_agent = state
        .get::<ares_store::PostgresClient>()
        .expect("not provided")
        .get_user_agent_by_name(&user_id, &agent_name)
        .await?;
    let public_agent = state
        .get::<ares_store::PostgresClient>()
        .expect("not provided")
        .get_public_agent_by_name(&agent_name)
        .await?;
    let config = state
        .get::<crate::overlay::AresConfigManager>()
        .expect("not provided")
        .config();
    let system_config = config.get_agent(&agent_name);
    let now = chrono::Utc::now().timestamp();
    resolve_from_candidates(user_agent, public_agent, system_config, &agent_name, now)
}

/// Row shape returned by [`UserAgentStore::list`].
#[derive(Debug, Clone, PartialEq)]
pub struct UserAgentRecord {
    /// Caller-scoped owner id (user or tenant, per provider).
    pub owner: String,
    /// Agent name unique within the owner scope.
    pub name: String,
    /// Display name.
    pub display_name: Option<String>,
    /// Short description.
    pub description: Option<String>,
    /// Model id.
    pub model: String,
    /// System prompt.
    pub system_prompt: Option<String>,
    /// Tool names.
    pub tools: Vec<String>,
    /// Max tool-call iterations.
    pub max_tool_iterations: i32,
    /// Whether tools may run in parallel.
    pub parallel_tools: bool,
    /// Whether the agent is public.
    pub is_public: bool,
    /// Extra provider-defined config.
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Create input accepted by [`UserAgentStore::create`].
#[derive(Debug, Clone, PartialEq)]
pub struct CreateUserAgent {
    /// Caller-scoped owner id (user or tenant, per provider).
    pub owner: String,
    /// Agent name unique within the owner scope.
    pub name: String,
    /// Display name.
    pub display_name: Option<String>,
    /// Short description.
    pub description: Option<String>,
    /// Model id.
    pub model: String,
    /// System prompt.
    pub system_prompt: Option<String>,
    /// Tool names.
    pub tools: Vec<String>,
    /// Max tool-call iterations.
    pub max_tool_iterations: i32,
    /// Whether tools may run in parallel.
    pub parallel_tools: bool,
    /// Whether the agent is public.
    pub is_public: bool,
    /// Extra provider-defined config.
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Update input accepted by [`UserAgentStore::update`].
/// `None` fields keep the stored value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpdateUserAgent {
    /// Display name (`Some(None)` clears).
    pub display_name: Option<Option<String>>,
    /// Short description (`Some(None)` clears).
    pub description: Option<Option<String>>,
    /// Model id.
    pub model: Option<String>,
    /// System prompt (`Some(None)` clears).
    pub system_prompt: Option<Option<String>>,
    /// Tool names.
    pub tools: Option<Vec<String>>,
    /// Max tool-call iterations.
    pub max_tool_iterations: Option<i32>,
    /// Whether tools may run in parallel.
    pub parallel_tools: Option<bool>,
    /// Whether the agent is public.
    pub is_public: Option<bool>,
    /// Extra provider-defined config.
    pub extra: Option<std::collections::HashMap<String, serde_json::Value>>,
}

/// Overridable backing store for the `/user/agents` endpoints.
///
/// Downstream binaries (for example ares-dirmacs) provide their own
/// implementation on the request [`Context`] before the router runs, for
/// example `ctx.provide(UserAgentStoreHandle::new(provider))`. With no
/// provider the endpoints keep today's responses (empty list / 501 for
/// writes), so the route table stays unchanged.
#[async_trait::async_trait]
pub trait UserAgentStore: Send + Sync + 'static {
    /// List agents visible to `owner`.
    async fn list(&self, owner: &str) -> crate::Result<Vec<UserAgentRecord>>;
    /// Create one agent; provider validates name/model conflicts.
    async fn create(&self, input: CreateUserAgent) -> crate::Result<UserAgentRecord>;
    /// Fetch one agent by owner + name.
    async fn get(&self, owner: &str, name: &str) -> crate::Result<UserAgentRecord>;
    /// Update one agent by owner + name.
    async fn update(
        &self,
        owner: &str,
        name: &str,
        patch: UpdateUserAgent,
    ) -> crate::Result<UserAgentRecord>;
    /// Delete one agent by owner + name. Returns true when a row was removed.
    async fn delete(&self, owner: &str, name: &str) -> crate::Result<bool>;
    /// Import one agent from a TOON document.
    async fn import_toon(&self, owner: &str, toon: &str) -> crate::Result<UserAgentRecord>;
    /// Export one agent as a TOON document.
    async fn export_toon(&self, owner: &str, name: &str) -> crate::Result<String>;
}

/// Cordis handle for the process-wide [`UserAgentStore`].
#[derive(Clone)]
pub struct UserAgentStoreHandle(pub Arc<dyn UserAgentStore>);

impl cordis::Service for UserAgentStoreHandle {
    fn name(&self) -> &'static str {
        "user_agent_store"
    }
    fn init(&self, _ctx: &Arc<cordis::Context>) -> cordis::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool {
        true
    }
}

impl UserAgentStoreHandle {
    /// Wrap a provider as a cordis service handle.
    pub fn new(inner: Arc<dyn UserAgentStore>) -> Self {
        Self(inner)
    }
    /// Borrow the inner provider.
    pub fn inner(&self) -> &Arc<dyn UserAgentStore> {
        &self.0
    }
}

/// Default backing store: preserves today's stub behavior.
struct DefaultUserAgentStore;

#[async_trait::async_trait]
impl UserAgentStore for DefaultUserAgentStore {
    async fn list(&self, _owner: &str) -> crate::Result<Vec<UserAgentRecord>> {
        Ok(Vec::new())
    }
    async fn create(&self, _input: CreateUserAgent) -> crate::Result<UserAgentRecord> {
        Err(crate::HttpError::from(AppError::Unavailable(
            "user agent store is not provided on this context".into(),
        )))
    }
    async fn get(&self, _owner: &str, name: &str) -> crate::Result<UserAgentRecord> {
        Err(crate::HttpError::from(AppError::NotFound(format!(
            "Agent '{name}' not found"
        ))))
    }
    async fn update(
        &self,
        _owner: &str,
        name: &str,
        _patch: UpdateUserAgent,
    ) -> crate::Result<UserAgentRecord> {
        Err(crate::HttpError::from(AppError::NotFound(format!(
            "Agent '{name}' not found"
        ))))
    }
    async fn delete(&self, _owner: &str, _name: &str) -> crate::Result<bool> {
        Ok(false)
    }
    async fn import_toon(&self, _owner: &str, _toon: &str) -> crate::Result<UserAgentRecord> {
        Err(crate::HttpError::from(AppError::Unavailable(
            "user agent store is not provided on this context".into(),
        )))
    }
    async fn export_toon(&self, _owner: &str, name: &str) -> crate::Result<String> {
        Err(crate::HttpError::from(AppError::NotFound(format!(
            "Agent '{name}' not found"
        ))))
    }
}

fn user_agent_store(ctx: &Arc<Context>) -> Arc<dyn UserAgentStore> {
    ctx.get::<UserAgentStoreHandle>()
        .map(|handle| Arc::clone(handle.inner()))
        .unwrap_or_else(|| Arc::new(DefaultUserAgentStore))
}

fn user_agent_owner(ctx: &Arc<Context>, claims_sub: &str) -> String {
    ares_agent::user_id_from_ctx(ctx, claims_sub)
}

fn user_agent_record_response(record: UserAgentRecord) -> UserAgentResponse {
    let now = chrono::Utc::now().timestamp();
    UserAgentResponse {
        id: format!("{}-{}", record.owner, record.name),
        name: record.name,
        display_name: record.display_name,
        description: record.description,
        model: record.model,
        system_prompt: record.system_prompt,
        tools: record.tools,
        max_tool_iterations: record.max_tool_iterations,
        parallel_tools: record.parallel_tools,
        is_public: record.is_public,
        usage_count: 0,
        average_rating: None,
        created_at: now,
        updated_at: now,
    }
}

fn user_agent_create_input(owner: String, req: CreateUserAgentReq) -> CreateUserAgent {
    CreateUserAgent {
        owner,
        name: req.name,
        display_name: req.display_name,
        description: req.description,
        model: req.model,
        system_prompt: req.system_prompt,
        tools: req.tools,
        max_tool_iterations: req.max_tool_iterations,
        parallel_tools: req.parallel_tools,
        is_public: req.is_public,
        extra: req.extra,
    }
}

/// GET /user/agents — list agents visible to the caller.
/// Default (no provider): empty list, as before.
pub async fn list_agents(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
) -> crate::Result<Json<Vec<UserAgentResponse>>> {
    let owner = user_agent_owner(&ctx, &claims.sub);
    let records = user_agent_store(&ctx).list(&owner).await?;
    Ok(Json(
        records.into_iter().map(user_agent_record_response).collect(),
    ))
}

/// POST /user/agents — create one agent.
/// Default (no provider): 503, as before (no store accepted writes).
pub async fn create_agent(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Json(req): Json<CreateUserAgentReq>,
) -> crate::Result<Json<UserAgentResponse>> {
    let owner = user_agent_owner(&ctx, &claims.sub);
    let record = user_agent_store(&ctx)
        .create(user_agent_create_input(owner, req))
        .await?;
    Ok(Json(user_agent_record_response(record)))
}

/// POST /user/agents/import — import one agent from a TOON document.
/// Default (no provider): 503, as before (no store accepted writes).
pub async fn import_agent_toon(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    body: String,
) -> crate::Result<Json<UserAgentResponse>> {
    let owner = user_agent_owner(&ctx, &claims.sub);
    let record = user_agent_store(&ctx).import_toon(&owner, &body).await?;
    Ok(Json(user_agent_record_response(record)))
}

/// GET /user/agents/{name} — fetch one agent.
/// Default (no provider): 404, as before.
pub async fn get_agent(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Path(name): Path<String>,
) -> crate::Result<Json<UserAgentResponse>> {
    let owner = user_agent_owner(&ctx, &claims.sub);
    let record = user_agent_store(&ctx).get(&owner, &name).await?;
    Ok(Json(user_agent_record_response(record)))
}

/// PUT /user/agents/{name} — update one agent.
/// Default (no provider): 404, as before.
pub async fn update_agent(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Path(name): Path<String>,
    Json(req): Json<UpdateUserAgentReq>,
) -> crate::Result<Json<UserAgentResponse>> {
    let owner = user_agent_owner(&ctx, &claims.sub);
    let record = user_agent_store(&ctx)
        .update(&owner, &name, req.into_patch())
        .await?;
    Ok(Json(user_agent_record_response(record)))
}

/// DELETE /user/agents/{name} — delete one agent.
/// Default (no provider): 404, as before (no store holds the name).
pub async fn delete_agent(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Path(name): Path<String>,
) -> crate::Result<Response> {
    let owner = user_agent_owner(&ctx, &claims.sub);
    let removed = user_agent_store(&ctx).delete(&owner, &name).await?;
    if removed {
        Ok(axum::http::StatusCode::NO_CONTENT.into_response())
    } else {
        Err(crate::HttpError::from(AppError::NotFound(format!(
            "Agent '{name}' not found"
        ))))
    }
}

/// GET /user/agents/{name}/export — export one agent as TOON.
/// Default (no provider): 404, as before.
pub async fn export_agent_toon(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Path(name): Path<String>,
) -> crate::Result<String> {
    let owner = user_agent_owner(&ctx, &claims.sub);
    user_agent_store(&ctx).export_toon(&owner, &name).await
}

/// Partial update body for `PUT /user/agents/{name}`.
/// `None` fields keep the stored value; `Some(None)` clears an optional.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct UpdateUserAgentReq {
    /// Display name (`Some(None)` clears).
    pub display_name: Option<Option<String>>,
    /// Short description (`Some(None)` clears).
    pub description: Option<Option<String>>,
    /// Model id.
    pub model: Option<String>,
    /// System prompt (`Some(None)` clears).
    pub system_prompt: Option<Option<String>>,
    /// Tool names.
    pub tools: Option<Vec<String>>,
    /// Max tool-call iterations.
    pub max_tool_iterations: Option<i32>,
    /// Whether tools may run in parallel.
    pub parallel_tools: Option<bool>,
    /// Whether the agent is public.
    pub is_public: Option<bool>,
    /// Extra provider-defined config.
    pub extra: Option<std::collections::HashMap<String, serde_json::Value>>,
}

impl UpdateUserAgentReq {
    fn into_patch(self) -> UpdateUserAgent {
        UpdateUserAgent {
            display_name: self.display_name,
            description: self.description,
            model: self.model,
            system_prompt: self.system_prompt,
            tools: self.tools,
            max_tool_iterations: self.max_tool_iterations,
            parallel_tools: self.parallel_tools,
            is_public: self.is_public,
            extra: self.extra,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::AresConfig;

    const FIXED_NOW: i64 = 1_700_000_000;

    fn sample_user_agent(name: &str, user_id: &str) -> UserAgent {
        UserAgent {
            id: format!("ua-{name}"),
            user_id: user_id.into(),
            name: name.into(),
            display_name: Some("Display".into()),
            description: Some("Desc".into()),
            model: "gpt-4o".into(),
            system_prompt: Some("Be helpful".into()),
            tools: r#"["search","calc"]"#.into(),
            max_tool_iterations: 5,
            parallel_tools: true,
            extra: r#"{"tier":"pro"}"#.into(),
            is_public: false,
            usage_count: 10,
            rating_sum: 8,
            rating_count: 2,
            created_at: FIXED_NOW - 100,
            updated_at: FIXED_NOW - 50,
        }
    }

    fn sample_agent_config() -> AgentConfig {
        AgentConfig {
            model: "llama3".into(),
            system_prompt: Some("system prompt".into()),
            tools: vec!["search".into(), "calc".into()],
            allowed_tools: None,
            max_tool_iterations: 7,
            parallel_tools: true,
            extra: HashMap::new(),
            compaction_enabled: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
        }
    }

    fn minimal_overlay_config(agent_name: &str, cfg: AgentConfig) -> AresConfig {
        let mut config: AresConfig = toml::from_str(
            r#"
[server]
host = "127.0.0.1"
port = 3000

[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"

[database]
url = "postgres://localhost/ares"
"#,
        )
        .expect("parse minimal config");
        config.agents.insert(agent_name.to_string(), cfg);
        config
    }

    #[test]
    fn default_max_iterations_is_ten() {
        assert_eq!(default_max_iterations(), 10);
    }

    #[test]
    fn create_req_deserializes_minimal_json() {
        let json = r#"{"name":"my-bot","model":"gpt-4o","tools":[]}"#;
        let req: CreateUserAgentReq = serde_json::from_str(json).unwrap();
        assert_eq!(req.name, "my-bot");
        assert_eq!(req.model, "gpt-4o");
        assert!(req.tools.is_empty());
        assert_eq!(req.max_tool_iterations, 10);
        assert!(!req.parallel_tools);
        assert!(!req.is_public);
        assert!(req.extra.is_empty());
    }

    #[test]
    fn create_req_applies_explicit_fields() {
        let json = r#"{
            "name":"bot",
            "display_name":"My Bot",
            "description":"Does things",
            "model":"claude",
            "system_prompt":"You are helpful",
            "tools":["web","calc"],
            "max_tool_iterations": 3,
            "parallel_tools": true,
            "is_public": true,
            "extra": {"voice": "alloy"}
        }"#;
        let req: CreateUserAgentReq = serde_json::from_str(json).unwrap();
        assert_eq!(req.display_name.as_deref(), Some("My Bot"));
        assert_eq!(req.tools, vec!["web", "calc"]);
        assert_eq!(req.max_tool_iterations, 3);
        assert!(req.parallel_tools);
        assert!(req.is_public);
    }

    #[test]
    fn create_req_roundtrip_json() {
        let req = CreateUserAgentReq {
            name: "rt".into(),
            display_name: None,
            description: None,
            model: "m".into(),
            system_prompt: None,
            tools: vec!["t".into()],
            max_tool_iterations: 4,
            parallel_tools: false,
            is_public: true,
            extra: HashMap::from([("k".into(), serde_json::json!(1))]),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CreateUserAgentReq = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn create_req_debug_and_clone() {
        let req = CreateUserAgentReq {
            name: "x".into(),
            display_name: None,
            description: None,
            model: "m".into(),
            system_prompt: None,
            tools: vec![],
            max_tool_iterations: 10,
            parallel_tools: false,
            is_public: false,
            extra: HashMap::new(),
        };
        let cloned = req.clone();
        assert_eq!(req, cloned);
        assert!(format!("{req:?}").contains("x"));
    }

    #[test]
    fn user_agent_response_serializes_all_fields() {
        let resp = UserAgentResponse {
            id: "id-1".into(),
            name: "agent".into(),
            display_name: Some("D".into()),
            description: Some("desc".into()),
            model: "gpt-4o".into(),
            system_prompt: Some("prompt".into()),
            tools: vec!["a".into()],
            max_tool_iterations: 5,
            parallel_tools: true,
            is_public: false,
            usage_count: 3,
            average_rating: Some(4.5),
            created_at: 100,
            updated_at: 200,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"average_rating\":4.5"));
        assert!(json.contains("\"tools\":[\"a\"]"));
    }

    #[test]
    fn user_agent_response_serializes_null_optionals() {
        let resp = UserAgentResponse {
            id: "id".into(),
            name: "n".into(),
            display_name: None,
            description: None,
            model: "m".into(),
            system_prompt: None,
            tools: vec![],
            max_tool_iterations: 10,
            parallel_tools: false,
            is_public: true,
            usage_count: 0,
            average_rating: None,
            created_at: 1,
            updated_at: 2,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("null"));
    }

    #[test]
    fn user_agent_response_from_db_maps_fields() {
        let agent = sample_user_agent("router", "u1");
        let resp = user_agent_response_from_db(agent.clone());
        assert_eq!(resp.name, "router");
        assert_eq!(resp.tools, vec!["search", "calc"]);
        assert_eq!(resp.usage_count, 10);
    }

    #[test]
    fn user_agent_response_from_db_computes_average_rating() {
        let agent = sample_user_agent("r", "u");
        let resp = user_agent_response_from_db(agent);
        assert_eq!(resp.average_rating, Some(4.0));
    }

    #[test]
    fn user_agent_response_from_db_no_rating_when_unrated() {
        let mut agent = sample_user_agent("r", "u");
        agent.rating_count = 0;
        let resp = user_agent_response_from_db(agent);
        assert!(resp.average_rating.is_none());
    }

    #[test]
    fn from_user_agent_matches_helper() {
        let agent = sample_user_agent("a", "u");
        let from_impl: UserAgentResponse = agent.clone().into();
        let from_fn = user_agent_response_from_db(agent);
        assert_eq!(from_impl, from_fn);
    }

    #[test]
    fn parse_tools_field_valid_json_array() {
        assert_eq!(
            parse_tools_field(r#"["a","b"]"#),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn parse_tools_field_empty_array() {
        assert!(parse_tools_field("[]").is_empty());
    }

    #[test]
    fn parse_tools_field_invalid_json_returns_empty() {
        assert!(parse_tools_field("not-json").is_empty());
    }

    #[test]
    fn parse_extra_config_valid_object() {
        let map = parse_extra_config(r#"{"k":"v","n":1}"#);
        assert_eq!(map.get("k").and_then(|v| v.as_str()), Some("v"));
        assert_eq!(map.get("n").and_then(|v| v.as_i64()), Some(1));
    }

    #[test]
    fn parse_extra_config_invalid_returns_empty() {
        assert!(parse_extra_config("[]").is_empty());
        assert!(parse_extra_config("broken").is_empty());
    }

    #[test]
    fn resolve_prefers_user_over_community_and_system() {
        let u = sample_user_agent("router", "u1");
        let c = sample_user_agent("router", "");
        let cfg = sample_agent_config();
        let (got, src) =
            resolve_from_candidates(Some(u.clone()), Some(c), Some(&cfg), "router", FIXED_NOW)
                .unwrap();
        assert_eq!(src, "user");
        assert_eq!(got.id, u.id);
    }

    #[test]
    fn resolve_prefers_community_over_system() {
        let c = sample_user_agent("router", "");
        let cfg = sample_agent_config();
        let (got, src) =
            resolve_from_candidates(None, Some(c.clone()), Some(&cfg), "router", FIXED_NOW)
                .unwrap();
        assert_eq!(src, "community");
        assert_eq!(got.id, c.id);
    }

    #[test]
    fn resolve_falls_back_to_system_config() {
        let cfg = sample_agent_config();
        let (got, src) =
            resolve_from_candidates(None, None, Some(&cfg), "router", FIXED_NOW).unwrap();
        assert_eq!(src, "system");
        assert_eq!(got.id, "system-router");
        assert_eq!(got.model, "llama3");
        assert_eq!(got.tools, r#"["search","calc"]"#);
    }

    #[test]
    fn resolve_not_found_when_all_tiers_missing() {
        let err = resolve_from_candidates(None, None, None, "missing", FIXED_NOW).unwrap_err();
        match err.0 {
            AppError::NotFound(msg) => assert!(msg.contains("missing")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn system_agent_from_config_serializes_tools() {
        let cfg = AgentConfig {
            tools: vec!["only".into()],
            ..sample_agent_config()
        };
        let agent = system_agent_from_config("my-agent", &cfg, FIXED_NOW);
        assert_eq!(agent.tools, r#"["only"]"#);
        assert!(agent.is_public);
    }

    #[test]
    fn config_get_agent_matches_system_resolution() {
        let config = minimal_overlay_config("router", sample_agent_config());
        let cfg = config.get_agent("router").expect("agent in config");
        let (from_pure, _) =
            resolve_from_candidates(None, None, Some(cfg), "router", FIXED_NOW).unwrap();
        let from_helper = system_agent_from_config("router", cfg, FIXED_NOW);
        assert_eq!(from_pure.id, from_helper.id);
    }

    #[test]
    fn config_get_agent_returns_none_for_unknown_name() {
        let config = minimal_overlay_config("router", sample_agent_config());
        assert!(config.get_agent("ghost").is_none());
    }

    #[test]
    fn agent_config_serde_roundtrip_json() {
        let original = sample_agent_config();
        let json = serde_json::to_string(&original).unwrap();
        let decoded: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original.model, decoded.model);
        assert_eq!(original.tools, decoded.tools);
    }

    fn test_record(owner: &str, name: &str) -> UserAgentRecord {
        UserAgentRecord {
            owner: owner.into(),
            name: name.into(),
            display_name: Some("Display".into()),
            description: Some("Desc".into()),
            model: "gpt-4o".into(),
            system_prompt: Some("Be helpful".into()),
            tools: vec!["search".into()],
            max_tool_iterations: 5,
            parallel_tools: false,
            is_public: false,
            extra: HashMap::new(),
        }
    }

    struct StaticAgents(Vec<UserAgentRecord>);

    #[async_trait::async_trait]
    impl UserAgentStore for StaticAgents {
        async fn list(&self, _owner: &str) -> crate::Result<Vec<UserAgentRecord>> {
            Ok(self.0.clone())
        }
        async fn create(&self, input: CreateUserAgent) -> crate::Result<UserAgentRecord> {
            Ok(test_record(&input.owner, &input.name))
        }
        async fn get(&self, owner: &str, name: &str) -> crate::Result<UserAgentRecord> {
            Ok(test_record(owner, name))
        }
        async fn update(
            &self,
            owner: &str,
            name: &str,
            _patch: UpdateUserAgent,
        ) -> crate::Result<UserAgentRecord> {
            Ok(test_record(owner, name))
        }
        async fn delete(&self, _owner: &str, _name: &str) -> crate::Result<bool> {
            Ok(true)
        }
        async fn import_toon(&self, owner: &str, _toon: &str) -> crate::Result<UserAgentRecord> {
            Ok(test_record(owner, "imported"))
        }
        async fn export_toon(&self, _owner: &str, _name: &str) -> crate::Result<String> {
            Ok("agent: exported".into())
        }
    }

    fn root_ctx() -> Arc<Context> {
        Context::new_root()
    }

    fn test_user() -> AuthUser {
        AuthUser(ares_types::types::Claims {
            sub: "tester".into(),
            email: "tester@example.com".into(),
            exp: 0,
            iat: 0,
            jti: String::new(),
            tenant_id: None,
        })
    }

    #[tokio::test]
    async fn default_list_returns_empty() {
        let Json(agents) = list_agents(State(root_ctx()), test_user()).await.unwrap();
        assert!(agents.is_empty());
    }

    #[tokio::test]
    async fn default_get_returns_not_found() {
        let err = get_agent(State(root_ctx()), test_user(), Path("ghost".into()))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("ghost"));
    }

    #[tokio::test]
    async fn injected_store_list_is_used() {
        let ctx = root_ctx();
        ctx.provide(UserAgentStoreHandle::new(Arc::new(StaticAgents(vec![
            test_record("owner-1", "alpha"),
        ]))));
        let Json(agents) = list_agents(State(ctx), test_user()).await.unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "alpha");
        assert_eq!(agents[0].model, "gpt-4o");
    }
}
