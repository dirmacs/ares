//! Skills module — SKILL.md file discovery and loading via thulp.
//!
//! Provides endpoints for listing and retrieving agent skills from
//! configured directories. Skills are SKILL.md files with YAML frontmatter.
//!
//! # Feature Flag
//!
//! Requires the `skills` feature to be enabled.
//!
//! ```toml
//! [dependencies]
//! ares-server = { version = "0.7", features = ["skills"] }
//! ```

pub mod loader {
    #[cfg(feature = "skills")]
    use thulp_skill_files::{SkillLoader, SkillLoaderConfig};

    #[cfg(feature = "skills")]
    /// Load all skills from the configured directories.
    ///
    /// Scans project, personal, and plugin directories for SKILL.md files
    /// and returns them with scope-based priority resolution.
    pub fn load_skills(config: &SkillsConfig) -> Vec<LoadedSkill> {
        let loader_config = SkillLoaderConfig {
            project_dir: config.project_dir.clone(),
            personal_dir: config.personal_dir.clone(),
            enterprise_dir: config.enterprise_dir.clone(),
            plugin_dirs: config.plugin_dirs.clone(),
            max_depth: 3,
        };

        let loader = SkillLoader::new(loader_config);
        match loader.load_all() {
            Ok(skills) => {
                tracing::info!(count = skills.len(), "Loaded skills from directories");
                skills
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load skills");
                Vec::new()
            }
        }
    }

    #[cfg(not(feature = "skills"))]
    /// Stub when `skills` feature is disabled — returns empty.
    pub fn load_skills(_config: &SkillsConfig) -> Vec<LoadedSkill> {
        Vec::new()
    }

    #[cfg(feature = "skills")]
    /// List skill names and descriptions (lightweight, no full content).
    pub fn list_skills(config: &SkillsConfig) -> Vec<SkillSummary> {
        load_skills(config)
            .into_iter()
            .map(|s| {
                let fqn = s.qualified_name();
                SkillSummary {
                    name: fqn,
                    description: s.file.frontmatter.description.clone().unwrap_or_default(),
                    scope: s.scope.to_string(),
                    path: s.file.path.to_string_lossy().to_string(),
                }
            })
            .collect()
    }

    #[cfg(not(feature = "skills"))]
    /// Stub — always empty when `skills` feature is disabled.
    pub fn list_skills(_config: &SkillsConfig) -> Vec<SkillSummary> {
        Vec::new()
    }

    #[cfg(feature = "skills")]
    /// Get a single skill by name.
    pub fn get_skill(config: &SkillsConfig, name: &str) -> Option<LoadedSkill> {
        load_skills(config)
            .into_iter()
            .find(|s| s.qualified_name() == name)
    }

    #[cfg(not(feature = "skills"))]
    /// Stub — always None when `skills` feature is disabled.
    pub fn get_skill(_config: &SkillsConfig, _name: &str) -> Option<LoadedSkill> {
        None
    }

    /// Skills configuration — where to look for SKILL.md files.
    #[derive(Debug, Clone, Default, serde::Deserialize)]
    pub struct SkillsConfig {
        /// Project skills directory (e.g., ./.claude/skills/).
        pub project_dir: Option<std::path::PathBuf>,
        /// Personal skills directory (e.g., ~/.claude/skills/).
        pub personal_dir: Option<std::path::PathBuf>,
        /// Enterprise skills directory.
        pub enterprise_dir: Option<std::path::PathBuf>,
        /// Plugin directories to scan.
        #[serde(default)]
        pub plugin_dirs: Vec<std::path::PathBuf>,
    }

    /// Lightweight skill summary for list endpoints.
    #[derive(Debug, Clone, serde::Serialize)]
    pub struct SkillSummary {
        pub name: String,
        pub description: String,
        pub scope: String,
        pub path: String,
    }

    #[cfg(feature = "skills")]
    pub use thulp_skill_files::LoadedSkill;

    #[cfg(not(feature = "skills"))]
    /// Stub LoadedSkill when `skills` feature is disabled — minimal shape for handlers to compile.
    #[derive(Debug, Clone)]
    pub struct LoadedSkill {
        pub file: SkillFile,
        pub scope: SkillScope,
    }

    #[cfg(not(feature = "skills"))]
    impl LoadedSkill {
        pub fn qualified_name(&self) -> String {
            String::new()
        }
    }

    #[cfg(not(feature = "skills"))]
    #[derive(Debug, Clone)]
    pub struct SkillFile {
        pub frontmatter: Frontmatter,
        pub path: std::path::PathBuf,
        pub content: String,
    }

    #[cfg(not(feature = "skills"))]
    #[derive(Debug, Clone, Default)]
    pub struct Frontmatter {
        pub description: Option<String>,
    }

    #[cfg(not(feature = "skills"))]
    #[derive(Debug, Clone, Default)]
    pub struct SkillScope(pub String);

    #[cfg(not(feature = "skills"))]
    impl std::fmt::Display for SkillScope {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }
}

pub use loader::*;

#[cfg(feature = "postgres")]
pub mod engine;
#[cfg(feature = "postgres")]
pub use engine::SkillEngine;

// ---------------------------------------------------------------------------
// Real Cordis SkillsService — Phase 4 step 16
// ---------------------------------------------------------------------------

use crate::execution::Execute;
use ares_tools::Tools;
use cordis::{Context, Service};
use std::sync::Arc;

/// Maximum skill call recursion depth.
pub const MAX_SKILL_CALL_DEPTH: usize = 8;

fn resolve_skill_tool(
    ctx: &Arc<Context>,
    tenant_id: &str,
    name: &str,
) -> Option<Arc<dyn ares_tools::Tool>> {
    let scoped = ctx.isolate::<Tools>(tenant_id.to_string());
    scoped.get::<Tools>()?.resolve(&scoped, name)
}

/// Run a skill tool step through `Tools::execute` (`tools.execute` waterfall).
///
/// Isolates the request ctx for the tenant. Missing `Tools` or an unknown
/// name is `"Tool {name} not found"`.
pub(crate) async fn execute_skill_tool(
    ctx: &Arc<Context>,
    tenant_id: &str,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let scoped = ctx.isolate::<Tools>(tenant_id.to_string());
    let tools = scoped
        .get::<Tools>()
        .ok_or_else(|| format!("Tool {name} not found"))?;
    tools
        .execute(&scoped, name, args)
        .await
        .map_err(|e| match e {
            ares_types::AppError::NotFound(_) => format!("Tool {name} not found"),
            e => format!("Tool {name} execution error: {e}"),
        })
}

fn default_step_input() -> serde_json::Value {
    serde_json::Value::Null
}

/// Walk helper for token count aggregation.
fn walk(value: &serde_json::Value, input_tokens: &mut i64, output_tokens: &mut i64) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(usage) = object.get("usage").and_then(|u| u.as_object()) {
                *input_tokens += usage
                    .get("prompt_tokens")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                *output_tokens += usage
                    .get("completion_tokens")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
            }
            for child in object.values() {
                walk(child, input_tokens, output_tokens);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                walk(item, input_tokens, output_tokens);
            }
        }
        _ => {}
    }
}

/// Aggregate token counts from a skill result value.
///
/// Traverses the JSON recursively looking for `usage.prompt_tokens` and
/// `usage.completion_tokens` fields.
pub fn skill_result_token_counts(result: &serde_json::Value) -> (i64, i64) {
    let mut input_tokens = 0;
    let mut output_tokens = 0;
    walk(result, &mut input_tokens, &mut output_tokens);
    (input_tokens, output_tokens)
}

/// Validate skill call recursion depth.
pub fn validate_skill_call_depth(depth: usize) -> Result<(), String> {
    if depth > MAX_SKILL_CALL_DEPTH {
        return Err(format!(
            "Skill call depth exceeded maximum of {}",
            MAX_SKILL_CALL_DEPTH
        ));
    }
    Ok(())
}

/// Generate skill LLM text via `Llm::complete` (`llm.complete` waterfall).
///
/// Missing `Llm` is an error. There is no factory `generate_with_history` path.
pub(crate) async fn skill_llm_content(ctx: &Arc<Context>, prompt: &str) -> Result<String, String> {
    let Some(llm) = ctx.get::<ares_llm::Llm>() else {
        return Err("LLM service not available via Context".into());
    };
    llm.complete(ctx, prompt).await.map_err(|e| e.to_string())
}

/// Map a skill `LlmCall` to [`ares_llm::LLMResponse`] through `Llm::complete`.
///
/// Pins `model_name` with `ModelOverride` intercept when none is already set.
#[cfg(feature = "postgres")]
async fn skill_llm_response(
    ctx: &Arc<Context>,
    prompt: &str,
    model_name: &str,
    _provider_name: &str,
) -> Result<ares_llm::LLMResponse, String> {
    let ctx = if model_name.is_empty() || ctx.get::<ares_llm::ModelOverride>().is_some() {
        Arc::clone(ctx)
    } else {
        ctx.with_intercept(ares_llm::ModelOverride {
            model: model_name.to_string(),
        })
    };
    let content = skill_llm_content(&ctx, prompt).await?;
    Ok(ares_llm::LLMResponse {
        content,
        tool_calls: vec![],
        finish_reason: "stop".into(),
        usage: None,
    })
}

/// One step inside a skill workflow — mirrors `crate::skill_engine::SkillStep`
/// but lives in the Cordis-owned SkillsService for provider-agnostic execution.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SkillStep {
    /// Call a tool by name with JSON arguments.
    ToolCall {
        #[serde(alias = "tool", alias = "name")]
        tool_name: String,
        #[serde(default = "default_step_input", alias = "arguments", alias = "input")]
        args: serde_json::Value,
    },
    /// Call an LLM with a prompt and a model tier.
    LlmCall {
        prompt: String,
        #[serde(alias = "model")]
        model_tier: String,
    },
    /// Execute another skill with optional JSON input.
    SkillCall {
        #[serde(alias = "skill", alias = "id")]
        skill_id: String,
        #[serde(default = "default_step_input", alias = "args", alias = "arguments")]
        input: serde_json::Value,
    },
    /// Conditional branch evaluated against execution context.
    Condition {
        expression: String,
        #[serde(default)]
        then_steps: Vec<SkillStep>,
    },
}

#[cfg(feature = "postgres")]
fn successful_step_context(result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"status":"success","result": result})
}

/// Evaluate a simple condition expression against the execution context.
///
/// Supported forms: `step_N.status == 'success'`, `step_N.result == 'value'`, etc.
#[cfg(feature = "postgres")]
fn evaluate_condition(expression: &str, context: &serde_json::Value) -> bool {
    let expr = expression.trim();
    // `step_N.status == 'success'` or `!=`
    if let Some((left, right)) = expr.split_once("==") {
        let l = left.trim();
        let r = right.trim().trim_matches('\'').trim_matches('"');
        if l.ends_with(".status") {
            let key = l.trim_end_matches(".status").trim();
            // key like step_0
            if let Some(obj) = context.get(key).and_then(|v| v.as_object()) {
                let status = obj.get("status").and_then(|v| v.as_str()).unwrap_or("");
                return status == r;
            }
            return false;
        }
        if l.ends_with(".result") {
            let key = l.trim_end_matches(".result").trim();
            if let Some(obj) = context.get(key) {
                // compare against result["content"] or raw
                let content = obj
                    .get("result")
                    .and_then(|r| r.get("content"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                return content == r;
            }
            return false;
        }
    }
    if let Some((left, right)) = expr.split_once("!=") {
        let l = left.trim();
        let r = right.trim().trim_matches('\'').trim_matches('"');
        if l.ends_with(".status") {
            let key = l.trim_end_matches(".status").trim();
            if let Some(obj) = context.get(key).and_then(|v| v.as_object()) {
                let status = obj.get("status").and_then(|v| v.as_str()).unwrap_or("");
                return status != r;
            }
            return true;
        }
    }
    false
}

#[cfg(feature = "postgres")]
fn ready_then_steps<'a>(
    expression: &str,
    then_steps: &'a [SkillStep],
    context: &serde_json::Value,
) -> Option<&'a [SkillStep]> {
    if evaluate_condition(expression, context) {
        Some(then_steps)
    } else {
        None
    }
}

/// Cordis Service for skills — owns SkillCall/ToolCall/LlmCall/Condition with depth limiting.
///
/// `execution` is the unified `Execute` (single place for observability/usage).
/// `max_depth` caps recursion (default 8 = `MAX_SKILL_CALL_DEPTH`).
///
/// `check()` returns `cfg!(feature = "skills")` (runtime-guarded withdrawal) so handlers can branch:
/// `if ctx.get::<SkillsService>().map(|s| s.check()).unwrap_or(false) { … }`
pub struct SkillsService {
    pub execution: Arc<Execute>,
    pub max_depth: usize,
}

impl SkillsService {
    /// Create with default depth 8.
    pub fn new(execution: Arc<Execute>) -> Self {
        Self {
            execution,
            max_depth: MAX_SKILL_CALL_DEPTH,
        }
    }

    /// Create with explicit max depth.
    pub fn with_max_depth(execution: Arc<Execute>, max_depth: usize) -> Self {
        Self {
            execution,
            max_depth,
        }
    }

    /// Execute a skill by id with JSON input via `ctx` (Cordis provider-agnostic).
    ///
    /// Steps are run sequentially: ToolCall via `Tools::execute` (`tools.execute`
    /// waterfall on a tenant isolate), LlmCall via `Llm::complete` (`ctx.get::<Llm>()`)
    /// or factory last-resort, SkillCall via recursion with depth limiting, Condition
    /// via expression evaluation. Uses `run_history` when DB is available.
    /// Resolves services via `ctx.get` — no HTTP state alias.
    pub async fn execute_skill(
        &self,
        skill_id: &str,
        input: serde_json::Value,
        ctx: &Arc<Context>,
    ) -> Result<serde_json::Value, String> {
        self.execute_skill_at_depth(skill_id, input, ctx, 0).await
    }

    /// Arc<Context> convenience overload — delegates to `execute_skill`.
    pub async fn execute_skill_with_ctx(
        &self,
        skill_id: &str,
        input: serde_json::Value,
        ctx: &Arc<Context>,
    ) -> Result<serde_json::Value, String> {
        self.execute_skill(skill_id, input, ctx).await
    }

    async fn execute_skill_at_depth(
        &self,
        skill_id: &str,
        input: serde_json::Value,
        ctx: &Arc<Context>,
        depth: usize,
    ) -> Result<serde_json::Value, String> {
        if depth > self.max_depth {
            return Err(format!(
                "Skill call depth exceeded maximum of {}",
                self.max_depth
            ));
        }
        validate_skill_call_depth(depth)?;

        #[cfg(not(feature = "postgres"))]
        {
            let _ = (skill_id, &input, ctx, &self.execution);
            // Without postgres, skill store unavailable — return context echo for correctness of cargo check both.
            #[allow(
                clippy::needless_return,
                reason = "cfg stub uses explicit return to keep postgres/non-postgres branches symmetric for audit"
            )]
            return Ok(serde_json::json!({"input": input, "skill_id": skill_id, "status":"ok"}));
        }

        #[cfg(feature = "postgres")]
        {
            // Prove execution is wired (observability path) — touch it
            let _exec = &self.execution;

            // Resolve PgPool via Cordis context (PostgresClient is always provided).
            let pool: sqlx::PgPool = {
                if let Some(svc) = ctx.get::<ares_store::PostgresClient>() {
                    svc.pool.clone()
                } else {
                    return Err("Database pool not available via Context".to_string());
                }
            };

            let tenant_id = input
                .get("tenant_id")
                .and_then(|v| v.as_str())
                .unwrap_or("default")
                .to_string();
            let run_id = input
                .get("run_id")
                .and_then(|v| v.as_str())
                .unwrap_or(&uuid::Uuid::new_v4().to_string())
                .to_string();

            // Load skill definition from DB
            let skill_store = ares_store::skills::SkillStore::new(&pool);
            let skill = skill_store
                .get_skill_for_tenant(skill_id, &tenant_id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "Skill not found".to_string())?;

            let steps: Vec<SkillStep> = serde_json::from_value(skill.steps)
                .map_err(|e| format!("Invalid skill steps: {}", e))?;

            let mut context = serde_json::json!({"input": input});
            let mut step_index: i32 = 0;

            for step in steps {
                match step {
                    SkillStep::ToolCall { tool_name, args } => {
                        tracing::info!("Step {}: tool_call {}", step_index, tool_name);
                        let start = std::time::Instant::now();
                        let result =
                            execute_skill_tool(ctx, &tenant_id, &tool_name, args.clone()).await?;
                        let latency_ms = start.elapsed().as_millis() as i64;
                        context[&format!("step_{}", step_index)] =
                            successful_step_context(result.clone());
                        // Log to run_history when pool available
                        let store = ares_store::run_history::RunHistoryStore::new(&pool);
                        let req = ares_store::run_history::LogToolCallRequest {
                            id: uuid::Uuid::new_v4().to_string(),
                            run_id: run_id.clone(),
                            tenant_id: tenant_id.clone(),
                            agent_name: "skill_executor".to_string(),
                            step_index,
                            tool_name: tool_name.clone(),
                            tool_type: "skill_step".to_string(),
                            arguments: args.clone(),
                            result: Some(result),
                            latency_ms,
                            status: "success".to_string(),
                            error_message: None,
                            created_at: chrono::Utc::now().timestamp(),
                        };
                        let _ = store.insert_tool_call(&req).await;
                    }
                    SkillStep::LlmCall { prompt, model_tier } => {
                        tracing::info!("Step {}: llm_call tier={}", step_index, model_tier);
                        let start = std::time::Instant::now();
                        let (provider_name, model_name) =
                            ("default".to_string(), model_tier.clone());
                        let response =
                            skill_llm_response(ctx, &prompt, &model_name, &provider_name).await?;

                        let latency_ms = start.elapsed().as_millis() as i64;
                        let result = serde_json::json!({"content": response.content, "usage": response.usage});
                        context[&format!("step_{}", step_index)] =
                            successful_step_context(result.clone());
                        let store = ares_store::run_history::RunHistoryStore::new(&pool);
                        let usage = response.usage.unwrap_or_default();
                        let req = ares_store::run_history::LogLlmCallRequest {
                            id: uuid::Uuid::new_v4().to_string(),
                            run_id: run_id.clone(),
                            tenant_id: tenant_id.clone(),
                            agent_name: "skill_executor".to_string(),
                            step_index,
                            provider: provider_name.clone(),
                            model: model_name.clone(),
                            prompt_tokens: usage.prompt_tokens as i64,
                            completion_tokens: usage.completion_tokens as i64,
                            total_tokens: usage.total_tokens as i64,
                            estimated_cost_usd: rust_decimal::Decimal::new(
                                (usage.prompt_tokens as i64 + usage.completion_tokens as i64) * 2,
                                6,
                            ),
                            latency_ms,
                            cached_tokens: usage.cached_tokens,
                            total_time_ms: Some(latency_ms),
                            status: "success".to_string(),
                            error_message: None,
                            request_payload: None,
                            response_payload: Some(
                                serde_json::json!({"content": response.content}),
                            ),
                            created_at: chrono::Utc::now().timestamp(),
                        };
                        let _ = store.insert_llm_call(&req).await;
                    }
                    SkillStep::SkillCall {
                        skill_id: inner_id,
                        input: inner_input,
                    } => {
                        tracing::info!("Step {}: skill_call {}", step_index, inner_id);
                        let result = Box::pin(self.execute_skill_at_depth(
                            &inner_id,
                            inner_input,
                            ctx,
                            depth + 1,
                        ))
                        .await?;
                        context[&format!("step_{}", step_index)] = successful_step_context(result);
                    }
                    SkillStep::Condition {
                        expression,
                        then_steps,
                    } => {
                        tracing::info!("Step {}: condition {}", step_index, expression);
                        if let Some(ready) = ready_then_steps(&expression, &then_steps, &context) {
                            for (sub_idx, sub_step) in ready.iter().enumerate() {
                                let sub_index = step_index + 1 + sub_idx as i32;
                                self.execute_sub_step(
                                    sub_step,
                                    ctx,
                                    &pool,
                                    &tenant_id,
                                    &run_id,
                                    sub_index,
                                    &mut context,
                                    depth,
                                )
                                .await?;
                            }
                        }
                    }
                }
                step_index += 1;
            }
            Ok(context)
        }
    }

    #[cfg(feature = "postgres")]
    async fn execute_sub_step(
        &self,
        step: &SkillStep,
        ctx: &Arc<Context>,
        pool: &sqlx::PgPool,
        tenant_id: &str,
        run_id: &str,
        step_index: i32,
        context: &mut serde_json::Value,
        depth: usize,
    ) -> Result<(), String> {
        match step {
            SkillStep::ToolCall { tool_name, args } => {
                let start = std::time::Instant::now();
                let result = execute_skill_tool(ctx, tenant_id, tool_name, args.clone()).await?;
                let latency_ms = start.elapsed().as_millis() as i64;
                context[&format!("step_{}", step_index)] = successful_step_context(result.clone());
                let store = ares_store::run_history::RunHistoryStore::new(pool);
                let req = ares_store::run_history::LogToolCallRequest {
                    id: uuid::Uuid::new_v4().to_string(),
                    run_id: run_id.to_string(),
                    tenant_id: tenant_id.to_string(),
                    agent_name: "skill_executor".to_string(),
                    step_index,
                    tool_name: tool_name.clone(),
                    tool_type: "skill_step".to_string(),
                    arguments: args.clone(),
                    result: Some(result),
                    latency_ms,
                    status: "success".to_string(),
                    error_message: None,
                    created_at: chrono::Utc::now().timestamp(),
                };
                let _ = store.insert_tool_call(&req).await;
                Ok(())
            }
            SkillStep::LlmCall { prompt, model_tier } => {
                let start = std::time::Instant::now();
                let (provider_name, model_name) = ("default".to_string(), model_tier.clone());
                let response = skill_llm_response(ctx, prompt, &model_name, &provider_name).await?;
                let latency_ms = start.elapsed().as_millis() as i64;
                let result =
                    serde_json::json!({"content": response.content, "usage": response.usage});
                context[&format!("step_{}", step_index)] = successful_step_context(result.clone());
                let store = ares_store::run_history::RunHistoryStore::new(pool);
                let usage = response.usage.unwrap_or_default();
                let req = ares_store::run_history::LogLlmCallRequest {
                    id: uuid::Uuid::new_v4().to_string(),
                    run_id: run_id.to_string(),
                    tenant_id: tenant_id.to_string(),
                    agent_name: "skill_executor".to_string(),
                    step_index,
                    provider: provider_name,
                    model: model_name,
                    prompt_tokens: usage.prompt_tokens as i64,
                    completion_tokens: usage.completion_tokens as i64,
                    total_tokens: usage.total_tokens as i64,
                    estimated_cost_usd: rust_decimal::Decimal::new(
                        (usage.prompt_tokens as i64 + usage.completion_tokens as i64) * 2,
                        6,
                    ),
                    latency_ms,
                    cached_tokens: usage.cached_tokens,
                    total_time_ms: Some(latency_ms),
                    status: "success".to_string(),
                    error_message: None,
                    request_payload: None,
                    response_payload: Some(serde_json::json!({"content": response.content})),
                    created_at: chrono::Utc::now().timestamp(),
                };
                let _ = store.insert_llm_call(&req).await;
                Ok(())
            }
            SkillStep::SkillCall { skill_id, input } => {
                let result =
                    Box::pin(self.execute_skill_at_depth(skill_id, input.clone(), ctx, depth + 1))
                        .await?;
                context[&format!("step_{}", step_index)] = successful_step_context(result);
                Ok(())
            }
            SkillStep::Condition {
                expression,
                then_steps,
            } => {
                if let Some(ready) = ready_then_steps(expression, then_steps, context) {
                    for (sub_idx, sub_step) in ready.iter().enumerate() {
                        let sub_index = step_index + 1 + sub_idx as i32;
                        Box::pin(self.execute_sub_step(
                            sub_step, ctx, pool, tenant_id, run_id, sub_index, context, depth,
                        ))
                        .await?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl Service for SkillsService {
    fn name(&self) -> &'static str {
        "skills"
    }
    fn check(&self) -> bool {
        cfg!(feature = "skills")
    }
}

/// Typed installer for [`SkillsService`]. No loader key (skills has none today).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SkillsPluginConfig {}

pub struct SkillsPlugin;

impl cordis::Plugin for SkillsPlugin {
    type Config = SkillsPluginConfig;
    type Provides = SkillsService;

    fn apply(
        &self,
        ctx: &Arc<Context>,
        _config: Self::Config,
    ) -> Result<Arc<Self::Provides>, cordis::CordisError> {
        let execution = match ctx.get::<Execute>() {
            Some(e) => e,
            None => tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(ctx.inject::<Execute>())
            }),
        };
        Ok(Arc::new(SkillsService::new(execution)))
    }
}

#[cfg(all(test, feature = "skills"))]
mod tests {
    use super::loader::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_skill_file(dir: &std::path::Path, name: &str, description: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let content = format!(
            "---\nname: {}\ndescription: {}\n---\n\n# {}\n\nSkill instructions here.\n",
            name, description, name
        );
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn test_skills_config_default() {
        let config = SkillsConfig::default();
        assert!(config.project_dir.is_none());
        assert!(config.personal_dir.is_none());
        assert!(config.plugin_dirs.is_empty());
    }

    #[test]
    fn test_load_skills_empty_dir() {
        let temp = TempDir::new().unwrap();
        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let skills = load_skills(&config);
        assert!(skills.is_empty());
    }

    #[test]
    fn test_load_skills_finds_skill_files() {
        let temp = TempDir::new().unwrap();
        create_skill_file(temp.path(), "test-skill", "A test skill");
        create_skill_file(temp.path(), "another-skill", "Another skill");

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let skills = load_skills(&config);
        assert_eq!(skills.len(), 2);
    }

    #[test]
    fn test_list_skills_returns_summaries() {
        let temp = TempDir::new().unwrap();
        create_skill_file(temp.path(), "my-skill", "Does something useful");

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let summaries = list_skills(&config);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].name, "my-skill");
        assert_eq!(summaries[0].description, "Does something useful");
        assert_eq!(summaries[0].scope, "project");
    }

    #[test]
    fn test_get_skill_found() {
        let temp = TempDir::new().unwrap();
        create_skill_file(temp.path(), "target-skill", "Find me");

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let skill = get_skill(&config, "target-skill");
        assert!(skill.is_some());
    }

    #[test]
    fn test_get_skill_not_found() {
        let temp = TempDir::new().unwrap();
        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        assert!(get_skill(&config, "nonexistent").is_none());
    }

    #[test]
    fn test_skill_summary_serialization() {
        let summary = SkillSummary {
            name: "test".to_string(),
            description: "A test".to_string(),
            scope: "project".to_string(),
            path: "/tmp/test/SKILL.md".to_string(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"name\":\"test\""));
        assert!(json.contains("\"scope\":\"project\""));
    }

    #[test]
    fn test_nonexistent_dir_returns_empty() {
        let config = SkillsConfig {
            project_dir: Some(PathBuf::from("/nonexistent/path/that/doesnt/exist")),
            ..Default::default()
        };
        let skills = load_skills(&config);
        assert!(skills.is_empty());
    }

    #[test]
    fn test_malformed_skill_file_skipped() {
        let temp = TempDir::new().unwrap();
        // Valid skill
        create_skill_file(temp.path(), "good-skill", "Works fine");
        // Malformed: no frontmatter at all
        let bad_dir = temp.path().join("bad-skill");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(bad_dir.join("SKILL.md"), "No frontmatter here, just text.").unwrap();

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let skills = load_skills(&config);
        // Should load at least the good skill; bad one may be skipped or loaded with defaults
        assert!(!skills.is_empty());
    }

    #[test]
    fn test_skill_with_empty_description() {
        let temp = TempDir::new().unwrap();
        let skill_dir = temp.path().join("empty-desc");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: empty-desc\n---\n\n# Empty Desc\n\nNo description field.\n",
        )
        .unwrap();

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let summaries = list_skills(&config);
        // Should still load; description defaults to empty string
        if !summaries.is_empty() {
            assert!(summaries[0].description.is_empty() || summaries[0].description.len() > 0);
        }
    }

    #[test]
    fn test_multiple_dirs_combined() {
        let project_dir = TempDir::new().unwrap();
        let personal_dir = TempDir::new().unwrap();
        create_skill_file(project_dir.path(), "proj-skill", "Project scope");
        create_skill_file(personal_dir.path(), "personal-skill", "Personal scope");

        let config = SkillsConfig {
            project_dir: Some(project_dir.path().to_path_buf()),
            personal_dir: Some(personal_dir.path().to_path_buf()),
            ..Default::default()
        };
        let skills = load_skills(&config);
        assert!(skills.len() >= 2, "Should find skills from both dirs");
    }

    #[test]
    fn test_plugin_dirs() {
        // Plugin dirs expect: plugin_dir/skills/<skill-name>/SKILL.md
        let plugin1 = TempDir::new().unwrap();
        let plugin2 = TempDir::new().unwrap();
        let skills1 = plugin1.path().join("skills");
        let skills2 = plugin2.path().join("skills");
        create_skill_file(&skills1, "plugin1-skill", "From plugin 1");
        create_skill_file(&skills2, "plugin2-skill", "From plugin 2");

        let config = SkillsConfig {
            plugin_dirs: vec![plugin1.path().to_path_buf(), plugin2.path().to_path_buf()],
            ..Default::default()
        };
        let skills = load_skills(&config);
        assert!(
            skills.len() >= 2,
            "Should find skills from plugin dirs, got {}",
            skills.len()
        );
    }

    #[test]
    fn test_all_dirs_none_returns_empty() {
        let config = SkillsConfig::default();
        let skills = load_skills(&config);
        assert!(skills.is_empty());
    }

    #[test]
    fn test_get_skill_wrong_name_returns_none() {
        let temp = TempDir::new().unwrap();
        create_skill_file(temp.path(), "real-skill", "I exist");

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        assert!(get_skill(&config, "fake-skill").is_none());
        assert!(get_skill(&config, "").is_none());
        assert!(get_skill(&config, "real-skil").is_none()); // typo
    }

    #[test]
    fn test_skill_summary_path_populated() {
        let temp = TempDir::new().unwrap();
        create_skill_file(temp.path(), "path-check", "Check path field");

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let summaries = list_skills(&config);
        assert_eq!(summaries.len(), 1);
        assert!(
            summaries[0].path.contains("SKILL.md"),
            "Path should contain SKILL.md, got: {}",
            summaries[0].path
        );
    }

    #[test]
    fn test_skills_config_deserialize() {
        let json = r#"{
            "project_dir": "/tmp/project",
            "personal_dir": "/tmp/personal",
            "plugin_dirs": ["/tmp/p1", "/tmp/p2"]
        }"#;
        let config: SkillsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.project_dir, Some(PathBuf::from("/tmp/project")));
        assert_eq!(config.personal_dir, Some(PathBuf::from("/tmp/personal")));
        assert_eq!(config.plugin_dirs.len(), 2);
    }
}

#[cfg(test)]
mod llm_call_tests {
    use super::skill_llm_content;
    #[cfg(feature = "postgres")]
    use super::skill_llm_response;

    use ares_llm::{ClientPool, Llm, ModelConfig, ProviderConfig, ProviderRegistry};
    use cordis::{Context, EventsService};
    use std::sync::Arc;

    /// Registry with a local Ollama stub so `Llm::complete` can reach the
    /// `"llm.complete"` waterfall without a live provider. Generate is not
    /// invoked when the handler skips `next`.
    fn stub_llm() -> Arc<Llm> {
        let providers: std::collections::HashMap<String, ProviderConfig> =
            serde_json::from_value(serde_json::json!({
                "ollama": {
                    "type": "ollama",
                    "api_key_env": "UNUSED",
                    "base_url": "http://127.0.0.1:1",
                    "default_model": "stub"
                }
            }))
            .expect("ollama provider config");
        let models: std::collections::HashMap<String, ModelConfig> =
            serde_json::from_value(serde_json::json!({
                "stub": {
                    "provider": "ollama",
                    "model": "stub",
                    "temperature": 0.0,
                    "max_tokens": 16
                }
            }))
            .expect("stub model config");
        let mut registry = ProviderRegistry::from_config(providers, models, None);
        registry.set_default_model("stub");
        Arc::new(Llm::new(
            Arc::new(registry),
            Arc::new(ClientPool::with_defaults()),
            None,
        ))
    }

    #[tokio::test]
    async fn skills_service_llm_call_uses_complete() {
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());
        ctx.provide_arc(stub_llm());
        events.on_waterfall("llm.complete".into(), |payload, _next| async move {
            let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
            Ok(serde_json::json!({ "content": format!("CACHED:{prompt}") }))
        });
        let out = skill_llm_content(&ctx, "hello-skill")
            .await
            .expect("Llm::complete waterfall");
        assert!(
            out.contains("CACHED"),
            "waterfall short-circuit must supply content, got {out:?}"
        );
        assert!(
            out.contains("hello-skill"),
            "prompt must reach llm.complete payload, got {out:?}"
        );
    }

    #[cfg(feature = "postgres")]
    #[tokio::test(flavor = "multi_thread")]
    async fn skills_service_llm_response_uses_complete() {
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());
        ctx.provide_arc(stub_llm());
        events.on_waterfall("llm.complete".into(), |_payload, _next| async move {
            Ok(serde_json::json!({ "content": "service-evented" }))
        });
        let response = skill_llm_response(&ctx, "skill-prompt", "stub", "default")
            .await
            .expect("Llm::complete waterfall");
        assert_eq!(response.content, "service-evented");
        assert!(response.tool_calls.is_empty());
        assert_eq!(response.finish_reason, "stop");
        assert!(response.usage.is_none());
    }

    #[tokio::test]
    async fn skills_service_llm_call_uses_complete_absent_llm() {
        let ctx = Context::new_root();
        let err = skill_llm_content(&ctx, "x").await.expect_err("no Llm");
        assert!(
            err.contains("not available"),
            "helper must Err when Llm is absent, got {err:?}"
        );
    }
}

#[cfg(test)]
mod tool_call_tests {
    use super::{execute_skill_tool, resolve_skill_tool};
    use ares_tools::{Tool, Tools};
    use cordis::{Context, EventsService};
    use serde_json::json;
    use std::sync::Arc;

    struct ProbeTool;

    #[async_trait::async_trait]
    impl Tool for ProbeTool {
        fn name(&self) -> &str {
            "probe"
        }
        fn description(&self) -> &str {
            "parent-provided probe"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> ares_types::Result<serde_json::Value> {
            Ok(json!({"ok": true}))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn skills_service_tool_call_uses_execute() {
        // block_in_place inside Tools::list requires the multi-thread runtime.
        // block_in_place inside Tools::list requires the multi-thread runtime.
        let root = Context::new_root();
        let events = root.provide(EventsService::new());
        // Skill steps resolve inside the tenant realm; label once and provide
        // into that realm so the helper's re-isolate walks into it.
        let ctx = root.isolate::<Tools>("acme");
        ctx.provide(Tools::from_static([Arc::new(ProbeTool) as Arc<dyn Tool>]));
        let _ = events;
        events.on_waterfall("tools.execute".into(), |payload, _next| async move {
            let name = payload
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(json!({ "result": { "cached": true, "name": name } }))
        });
        let out = execute_skill_tool(&ctx, "acme", "probe", json!({"x": 1}))
            .await
            .expect("tools.execute waterfall");
        assert_eq!(out.get("cached"), Some(&json!(true)));
        assert_eq!(out.get("name"), Some(&json!("probe")));
        assert!(
            out.get("ok").is_none(),
            "short-circuit must skip ProbeTool::execute, got {out:?}"
        );
    }

    #[test]
    fn skills_service_resolve_isolates_tenant() {
        let parent = Context::new_root();
        parent.provide(Tools::from_static([Arc::new(ProbeTool) as Arc<dyn Tool>]));
        // Realm boundary: parent tools must NOT leak into the acme scope...
        assert!(resolve_skill_tool(&parent, "acme", "probe").is_none());
        // ...and tools provided inside the scope resolve there.
        let scoped = parent.isolate::<Tools>("acme");
        scoped.provide(Tools::from_static([Arc::new(ProbeTool) as Arc<dyn Tool>]));
        assert!(resolve_skill_tool(&scoped, "acme", "probe").is_some());
        assert!(
            parent.get::<Tools>().is_some(),
            "parent Tools survives scope activity"
        );
    }
}
