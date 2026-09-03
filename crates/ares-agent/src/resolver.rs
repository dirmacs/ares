//! Tenant agent runtime resolver — 3-tier hierarchy: user → community → system config.

use crate::AgentConfig;
use ares_store::postgres::UserAgent;
use ares_store::traits::DatabaseClient;
use ares_types::types::{AppError, Result};

pub use crate::execution::AgentSource;

/// Build a synthetic system [`UserAgent`] from static TOML/TOON [`AgentConfig`].
pub fn system_agent_from_config(agent_name: &str, cfg: &AgentConfig, now: i64) -> UserAgent {
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
pub fn resolve_from_candidates(
    user_agent: Option<UserAgent>,
    public_agent: Option<UserAgent>,
    system_config: Option<&AgentConfig>,
    agent_name: &str,
    now: i64,
) -> Result<(UserAgent, AgentSource)> {
    if let Some(agent) = user_agent {
        return Ok((agent, AgentSource::User));
    }
    if let Some(agent) = public_agent {
        return Ok((agent, AgentSource::Community));
    }
    if let Some(cfg) = system_config {
        return Ok((
            system_agent_from_config(agent_name, cfg, now),
            AgentSource::System,
        ));
    }
    Err(AppError::NotFound(format!(
        "Agent '{agent_name}' not found"
    )))
}

/// Tenant identifier for per-tenant scoping (`ctx.isolate` label).
pub type TenantId = String;

use std::future::Future;
use std::sync::Arc;

use ares_store::TenantDb;
use cordis::{CordisError, Service};

use crate::registry::AgentRegistry;

/// Unified agent resolver — single place that resolves agents with ordered
/// precedence `tenant_db tenant_agents → community public → system AgentRegistry`.
///
/// Crate-private resolver. Production callers go through `Execute::run`.
/// Per-tenant isolation: `crate::tenant_scope(ctx, tenant_id)` (`Tools` + `Execute`).
pub(crate) struct Resolver {
    /// Tenant DB handle for `tenant_agents` and `public` lookups.
    pub tenant_db: Arc<TenantDb>,
    /// System registry (TOML/TOON) fallback.
    pub agent_registry: Arc<AgentRegistry>,
    /// Static config for system-tier fallback (optional when built from ctx).
    pub config: Option<Arc<std::collections::HashMap<String, AgentConfig>>>,
}

impl Resolver {
    /// Create a new resolver service.
    pub fn new(
        tenant_db: Arc<TenantDb>,
        agent_registry: Arc<AgentRegistry>,
        config: Arc<std::collections::HashMap<String, AgentConfig>>,
    ) -> Self {
        Self {
            tenant_db,
            agent_registry,
            config: Some(config),
        }
    }

    /// Build from `TenantDb` + `AgentRegistry` already on ctx (no factory provide).
    pub(crate) fn from_ctx(
        ctx: &Arc<cordis::Context>,
        agent_registry: Arc<AgentRegistry>,
    ) -> Option<Self> {
        Some(Self {
            tenant_db: ctx.get::<TenantDb>()?,
            agent_registry,
            config: None,
        })
    }

    /// Async 3-tier resolution: `tenant_db` user → community public → system config.
    ///
    /// Mirrors the free function `resolve_agent` but uses `self.tenant_db.pool()`
    /// via `sqlx` and `self.config`/`self.agent_registry` for the system tier.
    pub async fn resolve_async(
        &self,
        name: &str,
        user_id: &str,
    ) -> std::result::Result<(UserAgent, AgentSource), AppError> {
        let user_agent = sqlx::query_as::<_, UserAgent>(
            "SELECT * FROM user_agents WHERE user_id = $1 AND name = $2",
        )
        .bind(user_id)
        .bind(name)
        .fetch_optional(self.tenant_db.pool())
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;
        let public_agent = sqlx::query_as::<_, UserAgent>(
            "SELECT * FROM user_agents WHERE is_public = true AND name = $1 LIMIT 1",
        )
        .bind(name)
        .fetch_optional(self.tenant_db.pool())
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;
        let system_config_owned = self.agent_registry.get_config_any(name);
        let system_config = self
            .config
            .as_ref()
            .and_then(|c| c.get(name))
            .or(system_config_owned.as_ref());
        resolve_from_candidates(
            user_agent,
            public_agent,
            system_config,
            name,
            chrono::Utc::now().timestamp(),
        )
    }

    /// Resolve an agent by name using the isolate realm of `ctx` to derive the
    /// tenant label. Reads `ctx.isolate_label(TypeId::of::<crate::Execute>())`
    /// so per-tenant resolution flows through the Cordis isolate namespace rather
    /// than a throwaway method parameter.
    pub async fn resolve(
        &self,
        ctx: &Arc<cordis::Context>,
        name: &str,
    ) -> std::result::Result<(UserAgent, AgentSource), AppError> {
        let user_id = user_id_from_ctx(ctx, "");
        self.resolve_async(name, &user_id).await
    }
}

impl Service for Resolver {
    fn name(&self) -> &'static str {
        "Resolver"
    }

    fn init(
        &self,
        ctx: &Arc<cordis::Context>,
    ) -> std::pin::Pin<
        Box<
            dyn Future<
                    Output = std::result::Result<Option<Box<dyn cordis::Disposable>>, CordisError>,
                > + Send
                + '_,
        >,
    > {
        let ctx = ctx.clone();
        Box::pin(async move {
            let _ = ctx.inject::<crate::registry::AgentRegistry>().await;
            Ok(None)
        })
    }

    fn check(&self) -> bool {
        // Active when both sources are present; if DB unavailable, dependent
        // fibers should deactivate (guarded withdrawal).
        true
    }
}

/// Derive the effective user/tenant scope for agent resolution.
///
/// Precedence: isolate label for `Execute` (with a leading
/// `tenant:`/`user:` prefix stripped), then a non-empty
/// `ctx.get::<TenantContext>()` intercept id, then `fallback`.
pub fn user_id_from_ctx(ctx: &Arc<cordis::Context>, fallback: &str) -> String {
    crate::execution::user_id_from_ctx(ctx, fallback)
}

/// Resolve an agent for a user using the 3-tier hierarchy.
pub async fn resolve_agent(
    db: &dyn DatabaseClient,
    config: &std::collections::HashMap<String, AgentConfig>,
    user_id: &str,
    agent_name: String,
) -> Result<(UserAgent, String)> {
    let user_agent = db.get_user_agent_by_name(user_id, &agent_name).await?;
    let public_agent = db.get_public_agent_by_name(&agent_name).await?;
    let system_config = config.get(&agent_name);

    let (agent, source) = resolve_from_candidates(
        user_agent,
        public_agent,
        system_config,
        &agent_name,
        chrono::Utc::now().timestamp(),
    )?;
    Ok((agent, source.as_str().into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentConfig;
    use ares_store::traits::{ConversationSummary, DatabaseClient};
    use ares_types::types::{MemoryFact, Message, MessageRole, Preference};
    use async_trait::async_trait;
    use std::collections::HashMap;

    const FIXED_NOW: i64 = 1_700_000_000;

    fn user_agent(name: &str, user_id: &str) -> UserAgent {
        UserAgent {
            id: format!("ua-{name}"),
            user_id: user_id.into(),
            name: name.into(),
            display_name: Some(format!("{name} display")),
            description: None,
            model: "gpt-4o".into(),
            system_prompt: Some("user prompt".into()),
            tools: r#"["calc"]"#.into(),
            max_tool_iterations: 5,
            parallel_tools: true,
            extra: r#"{"k":"v"}"#.into(),
            is_public: false,
            usage_count: 1,
            rating_sum: 4,
            rating_count: 1,
            created_at: FIXED_NOW - 100,
            updated_at: FIXED_NOW - 50,
        }
    }

    fn agent_config() -> AgentConfig {
        AgentConfig {
            model: "llama3".into(),
            system_prompt: Some("system prompt".into()),
            tools: vec!["search".into(), "calc".into()],
            max_tool_iterations: 7,
            parallel_tools: true,
            allowed_tools: None,
            extra: HashMap::new(),
            compaction_enabled: None,
        }
    }

    fn minimal_overlay_config(
        agent_name: &str,
        cfg: AgentConfig,
    ) -> std::collections::HashMap<String, AgentConfig> {
        let mut config = std::collections::HashMap::new();
        config.insert(agent_name.to_string(), cfg);
        config
    }

    // ── Pure resolver logic ───────────────────────────────────────────────

    #[test]
    fn resolve_prefers_user_over_community_and_system() {
        let u = user_agent("router", "u1");
        let c = user_agent("router", "");
        let cfg = agent_config();
        let (got, src) =
            resolve_from_candidates(Some(u.clone()), Some(c), Some(&cfg), "router", FIXED_NOW)
                .unwrap();
        assert_eq!(src, AgentSource::User);
        assert_eq!(got.id, u.id);
    }

    #[test]
    fn resolve_prefers_community_over_system() {
        let c = user_agent("router", "");
        let cfg = agent_config();
        let (got, src) =
            resolve_from_candidates(None, Some(c.clone()), Some(&cfg), "router", FIXED_NOW)
                .unwrap();
        assert_eq!(src, AgentSource::Community);
        assert_eq!(got.id, c.id);
    }

    #[test]
    fn resolve_falls_back_to_system_config() {
        let cfg = agent_config();
        let (got, src) =
            resolve_from_candidates(None, None, Some(&cfg), "router", FIXED_NOW).unwrap();
        assert_eq!(src, AgentSource::System);
        assert_eq!(got.id, "system-router");
        assert_eq!(got.user_id, "system");
        assert_eq!(got.model, "llama3");
        assert_eq!(got.system_prompt.as_deref(), Some("system prompt"));
        assert_eq!(got.tools, r#"["search","calc"]"#);
        assert_eq!(got.max_tool_iterations, 7);
        assert!(got.parallel_tools);
        assert_eq!(got.created_at, FIXED_NOW);
        assert_eq!(got.updated_at, FIXED_NOW);
    }

    #[test]
    fn resolve_not_found_when_all_tiers_missing() {
        let err = resolve_from_candidates(None, None, None, "missing", FIXED_NOW).unwrap_err();
        match err {
            AppError::NotFound(msg) => assert!(msg.contains("missing")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn agent_source_as_str_labels() {
        assert_eq!(AgentSource::User.as_str(), "user");
        assert_eq!(AgentSource::Community.as_str(), "community");
        assert_eq!(AgentSource::System.as_str(), "system");
    }

    #[test]
    fn system_agent_from_config_serializes_tools_as_json() {
        let cfg = AgentConfig {
            tools: vec!["a".into()],
            ..agent_config()
        };
        let agent = system_agent_from_config("my-agent", &cfg, FIXED_NOW);
        assert_eq!(agent.name, "my-agent");
        assert_eq!(agent.tools, r#"["a"]"#);
        assert!(agent.is_public);
    }

    #[test]
    fn config_get_agent_matches_system_resolution() {
        let config = minimal_overlay_config("router", agent_config());
        let cfg = config.get("router").expect("agent in config");
        let (from_pure, _) =
            resolve_from_candidates(None, None, Some(cfg), "router", FIXED_NOW).unwrap();
        let from_helper = system_agent_from_config("router", cfg, FIXED_NOW);
        assert_eq!(from_pure.id, from_helper.id);
        assert_eq!(from_pure.model, from_helper.model);
    }

    #[test]
    fn config_get_agent_returns_none_for_unknown_name() {
        let config = minimal_overlay_config("router", agent_config());
        assert!(!config.contains_key("ghost"));
    }

    // ── Serde roundtrips ──────────────────────────────────────────────────

    fn assert_agent_config_eq(a: &AgentConfig, b: &AgentConfig) {
        assert_eq!(a.model, b.model);
        assert_eq!(a.system_prompt, b.system_prompt);
        assert_eq!(a.tools, b.tools);
        assert_eq!(a.max_tool_iterations, b.max_tool_iterations);
        assert_eq!(a.parallel_tools, b.parallel_tools);
        assert_eq!(a.extra, b.extra);
    }

    #[test]
    fn agent_config_serde_roundtrip_json() {
        let original = agent_config();
        let json = serde_json::to_string(&original).unwrap();
        let decoded: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_agent_config_eq(&original, &decoded);
    }

    #[test]
    fn agent_config_serde_roundtrip_toml() {
        let original = agent_config();
        let serialized = toml::to_string(&original).unwrap();
        let decoded: AgentConfig = toml::from_str(&serialized).unwrap();
        assert_agent_config_eq(&original, &decoded);
    }

    #[test]
    fn agent_source_serde_roundtrip() {
        for source in [
            AgentSource::User,
            AgentSource::Community,
            AgentSource::System,
        ] {
            let json = serde_json::to_string(&source).unwrap();
            let decoded: AgentSource = serde_json::from_str(&json).unwrap();
            assert_eq!(source, decoded);
        }
    }

    // ── Async integration (mock DB) ─────────────────────────────────────

    struct MockDb {
        user: Option<UserAgent>,
        public: Option<UserAgent>,
    }

    #[async_trait]
    impl DatabaseClient for MockDb {
        async fn create_user(&self, _: &str, _: &str, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        async fn get_user_by_email(&self, _: &str) -> Result<Option<ares_store::traits::User>> {
            Ok(None)
        }
        async fn get_user_by_id(&self, _: &str) -> Result<Option<ares_store::traits::User>> {
            Ok(None)
        }
        async fn create_session(&self, _: &str, _: &str, _: &str, _: i64) -> Result<()> {
            Ok(())
        }
        async fn validate_session(&self, _: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn delete_session(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn delete_session_by_token_hash(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn create_conversation(&self, _: &str, _: &str, _: Option<&str>) -> Result<()> {
            Ok(())
        }
        async fn conversation_exists(&self, _: &str) -> Result<bool> {
            Ok(false)
        }
        async fn get_user_conversations(&self, _: &str) -> Result<Vec<ConversationSummary>> {
            Ok(vec![])
        }
        async fn get_conversation(&self, _: &str) -> Result<ares_store::postgres::Conversation> {
            Err(AppError::NotFound("conversation".into()))
        }
        async fn delete_conversation(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn update_conversation_title(&self, _: &str, _: Option<&str>) -> Result<()> {
            Ok(())
        }
        async fn add_message(&self, _: &str, _: &str, _: MessageRole, _: &str) -> Result<()> {
            Ok(())
        }
        async fn get_conversation_history(&self, _: &str) -> Result<Vec<Message>> {
            Ok(vec![])
        }
        async fn store_memory_fact(&self, _: &MemoryFact) -> Result<()> {
            Ok(())
        }
        async fn get_user_memory(&self, _: &str) -> Result<Vec<MemoryFact>> {
            Ok(vec![])
        }
        async fn get_memory_by_category(&self, _: &str, _: &str) -> Result<Vec<MemoryFact>> {
            Ok(vec![])
        }
        async fn store_preference(&self, _: &str, _: &Preference) -> Result<()> {
            Ok(())
        }
        async fn get_user_preferences(&self, _: &str) -> Result<Vec<Preference>> {
            Ok(vec![])
        }
        async fn get_preference(&self, _: &str, _: &str, _: &str) -> Result<Option<Preference>> {
            Ok(None)
        }
        async fn get_user_agent_by_name(&self, _: &str, _: &str) -> Result<Option<UserAgent>> {
            Ok(self.user.clone())
        }
        async fn get_public_agent_by_name(&self, _: &str) -> Result<Option<UserAgent>> {
            Ok(self.public.clone())
        }
        async fn list_user_agents(&self, _: &str) -> Result<Vec<UserAgent>> {
            Ok(vec![])
        }
        async fn list_public_agents(&self, _: u32, _: u32) -> Result<Vec<UserAgent>> {
            Ok(vec![])
        }
        async fn create_user_agent(&self, _: &UserAgent) -> Result<()> {
            Ok(())
        }
        async fn update_user_agent(&self, _: &UserAgent) -> Result<()> {
            Ok(())
        }
        async fn delete_user_agent(&self, _: &str, _: &str) -> Result<bool> {
            Ok(false)
        }
    }

    #[tokio::test]
    async fn resolve_agent_user_tier_via_db() {
        let db = MockDb {
            user: Some(user_agent("router", "u1")),
            public: Some(user_agent("router", "")),
        };
        let config = minimal_overlay_config("router", agent_config());
        let (agent, source) = resolve_agent(&db, &config, "u1", "router".into())
            .await
            .unwrap();
        assert_eq!(source, "user");
        assert_eq!(agent.user_id, "u1");
    }

    #[tokio::test]
    async fn resolve_agent_system_tier_from_config() {
        let db = MockDb {
            user: None,
            public: None,
        };
        let config = minimal_overlay_config("router", agent_config());
        let (agent, source) = resolve_agent(&db, &config, "u1", "router".into())
            .await
            .unwrap();
        assert_eq!(source, "system");
        assert_eq!(agent.id, "system-router");
    }

    #[tokio::test]
    async fn resolve_agent_not_found_error_path() {
        let db = MockDb {
            user: None,
            public: None,
        };
        let config = minimal_overlay_config("other", agent_config());
        let err = resolve_agent(&db, &config, "u1", "missing".into())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[cfg(feature = "postgres")]
    #[tokio::test]
    async fn resolve_uses_isolate_label_for_scope() {
        // Cordis design (§4): the tenant scope for agent resolution is derived
        // from the context's isolate namespace, not a throwaway method param.
        // A tenant-isolated context ("tenant:acme") must make the resolver
        // resolve the user tier under that tenant label. The pure helper
        // `user_id_from_ctx` drives the DB-aware 3-tier resolution: it reads
        // ctx.isolate_label(TypeId::of::<crate::Execute>()) and strips the
        // "tenant:" prefix to yield the effective user scope.

        let ctx: Arc<cordis::Context> = cordis::Context::new_root();
        let fallback = "anon";
        // Untagged context -> falls back to the provided default.
        assert_eq!(
            user_id_from_ctx(&ctx, fallback),
            "anon",
            "no isolate label -> fallback scope"
        );

        let tenant_ctx = crate::tenant_scope(&ctx, "acme");
        assert_eq!(
            user_id_from_ctx(&tenant_ctx, fallback),
            "acme",
            "tenant_scope('acme') -> scope 'acme'"
        );

        let user_ctx = ctx.isolate::<crate::Execute>("user:u42");
        assert_eq!(
            user_id_from_ctx(&user_ctx, fallback),
            "u42",
            "isolate label 'user:u42' -> scope 'u42'"
        );
    }

    #[test]
    fn user_id_from_ctx_reads_tenant_context_intercept() {
        use ares_types::models::{TenantContext, TenantTier};

        let root: Arc<cordis::Context> = cordis::Context::new_root();
        let ctx = root.with_intercept(TenantContext::new("acme".into(), TenantTier::Pro));
        assert_eq!(user_id_from_ctx(&ctx, "anon"), "acme");
    }

    #[test]
    fn user_id_from_ctx_isolate_label_wins_over_intercept() {
        use ares_types::models::{TenantContext, TenantTier};

        let root: Arc<cordis::Context> = cordis::Context::new_root();
        let intercepted =
            root.with_intercept(TenantContext::new("from-intercept".into(), TenantTier::Pro));
        let isolated = crate::tenant_scope(&intercepted, "from-isolate");
        assert_eq!(user_id_from_ctx(&isolated, "anon"), "from-isolate");
    }
}
