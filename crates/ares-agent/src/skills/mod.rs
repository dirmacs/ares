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

/// Step kinds allowed inside a delegated (nested) sub-workflow.
///
/// Delegation itself (`skill_call`) is deliberately excluded: a child skill
/// runs model-driven steps only, so a looping or compromised child cannot
/// re-enter delegation from within delegated execution.
#[cfg(feature = "postgres")]
pub const DELEGATED_SUB_TOOL_ALLOWLIST: [&str; 3] = ["tool_call", "llm_call", "condition"];

/// Enforce the delegated sub-tool allowlist at `depth > 0`.
///
/// Top-level executions (`depth == 0`) delegate freely; deeper ones may only
/// run allowlisted step shapes. Violations fail with the stable
/// `delegated_step_not_allowed:` marker so callers can classify the refusal.
#[cfg(feature = "postgres")]
pub fn validate_delegated_step(kind: &str, depth: usize) -> Result<(), String> {
    if depth == 0 || DELEGATED_SUB_TOOL_ALLOWLIST.contains(&kind) {
        return Ok(());
    }
    Err(format!(
        "delegated_step_not_allowed: {kind} may not run inside a delegated \
         sub-workflow at depth {depth}; allowed kinds: {}",
        DELEGATED_SUB_TOOL_ALLOWLIST.join(", ")
    ))
}

/// Hard cap on tool rounds executed by ONE skill execution (main steps plus
/// conditional branches). Consuming one more round than the cap aborts the
/// execution instead of looping.
#[cfg(feature = "postgres")]
pub const MAX_NESTED_TOOL_ROUNDS: usize = 3;

/// Refuse the round that would exceed [`MAX_NESTED_TOOL_ROUNDS`].
///
/// `rounds` counts tool rounds already consumed by this skill execution;
/// exactly [`MAX_NESTED_TOOL_ROUNDS`] may run, the next attempt aborts with
/// the stable `tool_round_cap_exceeded:` prefix so callers can classify it.
#[cfg(feature = "postgres")]
pub fn check_tool_round_cap(rounds: usize, scope: &str) -> Result<(), String> {
    if rounds < MAX_NESTED_TOOL_ROUNDS {
        return Ok(());
    }
    Err(format!(
        "tool_round_cap_exceeded: {scope} exceeded the hard cap of \
         {MAX_NESTED_TOOL_ROUNDS} nested tool rounds"
    ))
}

/// Hard cap on parallel delegated tasks accepted by
/// [`parse_delegation_args`].
///
/// Reuses the existing configured recursion limit: each parallel task
/// becomes its own delegated skill call one level deeper, so the depth cap
/// is the natural ceiling.
#[cfg(feature = "postgres")]
pub const MAX_PARALLEL_DELEGATED_TASKS: usize = MAX_SKILL_CALL_DEPTH;

/// Parsed delegation argument flags and per-task tokens.
#[cfg(feature = "postgres")]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DelegationArgs {
    /// One token vector per task. Before `--parallel` all plain tokens form
    /// one task split on bare `|` separators; once `--parallel` latches,
    /// every following plain token starts its own task and `|` is ignored.
    pub tasks: Vec<Vec<String>>,
    /// `--parallel` mode latch.
    pub parallel: bool,
    /// Value consumed from exactly one token after `--model`.
    pub model: Option<String>,
    /// `--tools` boolean enabling the inner tool loop for delegated tasks.
    pub tools: bool,
}

impl DelegationArgs {
    /// Model resolution precedence: explicit flag > profile default >
    /// global default. An empty profile default falls through to the global
    /// default.
    pub fn resolved_model(&self, profile_default: &str, global_default: &str) -> String {
        match (&self.model, profile_default.is_empty()) {
            (Some(model), _) => model.clone(),
            (None, false) => profile_default.to_string(),
            (None, true) => global_default.to_string(),
        }
    }
}

/// Quote-aware tokenizer: a double-quoted segment is one token that may
/// contain spaces and `|` separators; inside quotes a backslash escapes the
/// next character.
#[cfg(feature = "postgres")]
pub fn tokenize_delegation_args(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    for ch in input.chars() {
        if in_quotes && escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            c if !in_quotes && c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Parse a delegation argument string into flags plus per-task tokens.
///
/// Flags: `--parallel` (mode latch — tokens after it become separate tasks
/// and `|` separators are ignored), `--model <value>` (consumes exactly one
/// token), `--tools` (boolean enabling the inner tool loop). Everything
/// else is task text; bare `|` separates sequential tasks before the latch.
#[cfg(feature = "postgres")]
pub fn parse_delegation_args(input: &str) -> Result<DelegationArgs, String> {
    let tokens = tokenize_delegation_args(input);
    let mut args = DelegationArgs::default();
    args.tasks.push(Vec::new());
    let mut latched = false;
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "--parallel" => {
                args.parallel = true;
                latched = true;
                args.tasks.push(Vec::new());
            }
            "--model" => {
                index += 1;
                let value = tokens
                    .get(index)
                    .ok_or_else(|| "--model consumes exactly one value; none given".to_string())?;
                args.model = Some(value.clone());
            }
            "--tools" => args.tools = true,
            "|" if !latched => args.tasks.push(Vec::new()),
            // '|' separators are ignored once --parallel latched.
            "|" => {}
            plain if latched => args.tasks.push(vec![plain.to_string()]),
            plain => args
                .tasks
                .last_mut()
                .expect("seed task bucket always present")
                .push(plain.to_string()),
        }
        index += 1;
    }
    // Drop empty buckets left by leading/trailing/doubled separators.
    args.tasks.retain(|task| !task.is_empty());
    if args.tasks.len() > MAX_PARALLEL_DELEGATED_TASKS {
        args.tasks.truncate(MAX_PARALLEL_DELEGATED_TASKS);
    }
    Ok(args)
}

/// Strip tool-chatter lines from delegated result text before it enters the
/// parent context.
///
/// A chatter line is any line whose first non-whitespace character is `/`
/// (slash-command style tool output). Slashes mid-line stay untouched; text
/// without chatter is returned unchanged (no reallocation).
#[cfg(feature = "postgres")]
pub fn sanitize_result_text(text: &str) -> String {
    let total = text.lines().count();
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('/'))
        .collect();
    if kept.len() == total {
        return text.to_string();
    }
    kept.join("\n")
}

/// Sanitize every string leaf of a nested result value in place.
#[cfg(feature = "postgres")]
pub fn sanitize_result_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = sanitize_result_text(text),
        serde_json::Value::Array(items) => {
            for item in items {
                sanitize_result_value(item);
            }
        }
        serde_json::Value::Object(map) => {
            for child in map.values_mut() {
                sanitize_result_value(child);
            }
        }
        _ => {}
    }
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
        reasoning_content: None,
        response_id: None,
    })
}

/// Fixed cache-friendly preamble for the delegated-result reviewer prompt.
///
/// Kept byte-stable across calls so provider-side prompt caches hit on the
/// template; only the per-call tail (skill id, input, result) varies.
#[cfg(feature = "postgres")]
const DELEGATED_REVIEW_TEMPLATE: &str = "You are reviewing the result of a delegated sub-workflow step.\n\
                                         Judge the result for consistency with the requested input \
                                         and overall task fit.\n\
                                         Reply with ACCEPT or REJECT on the first line, then one \
                                         short sentence of review notes.\n";

/// Outcome of the delegated-result review micro-step.
#[cfg(feature = "postgres")]
struct ReviewVerdict {
    accepted: bool,
    notes: String,
}

/// Parse an ACCEPT/REJECT verdict plus trailing review notes from reviewer text.
///
/// The first line must carry an explicit `ACCEPT` or `REJECT` word; anything
/// else is an error so callers degrade to pass-through exactly like a
/// reviewer outage instead of guessing.
#[cfg(feature = "postgres")]
fn parse_review_verdict(content: &str) -> Result<ReviewVerdict, String> {
    let mut lines = content.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines
        .next()
        .ok_or_else(|| "empty reviewer response".to_string())?;
    let start = first
        .char_indices()
        .find(|(_, c)| c.is_ascii_alphabetic())
        .map(|(i, _)| i)
        .ok_or_else(|| format!("unparsable reviewer verdict: {first}"))?;
    let end = start
        + first[start..]
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(first.len() - start);
    let word = &first[start..end];
    // Notes: whatever follows the verdict word on the same line, then any
    // further lines.
    let mut parts: Vec<&str> = Vec::new();
    let rest_of_first = first[end..]
        .trim()
        .trim_start_matches([':', ';', '-', ','])
        .trim();
    if !rest_of_first.is_empty() {
        parts.push(rest_of_first);
    }
    parts.extend(lines);
    let notes = parts.join(" ");
    match word.to_ascii_uppercase().as_str() {
        "ACCEPT" => Ok(ReviewVerdict {
            accepted: true,
            notes,
        }),
        "REJECT" => Ok(ReviewVerdict {
            accepted: false,
            notes,
        }),
        _ => Err(format!("unparsable reviewer verdict: {first}")),
    }
}

/// Run the review micro-step for a delegated sub-workflow result through
/// `Llm::complete` (`llm.complete` waterfall). An error means the reviewer
/// is unavailable — callers degrade silently to pass-through.
#[cfg(feature = "postgres")]
async fn review_delegated_result(
    ctx: &Arc<Context>,
    delegated_skill_id: &str,
    delegated_input: &serde_json::Value,
    result: &serde_json::Value,
) -> Result<ReviewVerdict, String> {
    let prompt = format!(
        "{}Delegated sub-workflow: {delegated_skill_id}\nRequested input: \
         {delegated_input}\nResult to review: {result}",
        DELEGATED_REVIEW_TEMPLATE
    );
    let content = skill_llm_content(ctx, &prompt).await?;
    parse_review_verdict(&content)
}

/// One step inside a skill workflow — mirrors `crate::skill_engine::SkillStep»
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
    /// Opt-in delegated-result review gate (default off). When on, nested
    /// `SkillCall` results pass a review micro-step before integration.
    pub review_delegated_results: bool,
}

impl SkillsService {
    /// Create with default depth 8 and the review gate off.
    pub fn new(execution: Arc<Execute>) -> Self {
        Self {
            execution,
            max_depth: MAX_SKILL_CALL_DEPTH,
            review_delegated_results: false,
        }
    }

    /// Create with explicit max depth; the review gate stays off.
    pub fn with_max_depth(execution: Arc<Execute>, max_depth: usize) -> Self {
        Self {
            execution,
            max_depth,
            review_delegated_results: false,
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
            // Per-execution tool-round ledger (main steps + conditional branches).
            let mut tool_rounds: usize = 0;

            for step in steps {
                match step {
                    SkillStep::ToolCall { tool_name, args } => {
                        check_tool_round_cap(tool_rounds, &format!(
                            "skill {skill_id} main step {step_index}"
                        ))?;
                        tool_rounds += 1;
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
                        // Anti-recursion: delegation may not nest inside a
                        // delegated sub-workflow.
                        validate_delegated_step("skill_call", depth)?;
                        // Snapshot the input for review BEFORE the consuming
                        // sub-execution; no allocation while the gate is off.
                        let review_input =
                            self.review_delegated_results.then(|| inner_input.clone());
                        let result = Box::pin(self.execute_skill_at_depth(
                            &inner_id,
                            inner_input,
                            ctx,
                            depth + 1,
                        ))
                        .await?;
                        // Tool hygiene: strip tool-chatter command lines from
                        // delegated text before it enters the parent context.
                        let mut result = result;
                        sanitize_result_value(&mut result);
                        let result = match review_input {
                            Some(review_input) => {
                                self.review_nested_result(
                                    &inner_id,
                                    &review_input,
                                    result,
                                    ctx,
                                )
                                .await?
                            }
                            None => result,
                        };
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
                                    &mut tool_rounds,
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
    #[allow(clippy::too_many_arguments)]
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
        tool_rounds: &mut usize,
    ) -> Result<(), String> {
        match step {
            SkillStep::ToolCall { tool_name, args } => {
                check_tool_round_cap(
                    *tool_rounds,
                    &format!("skill sub-step {step_index}"),
                )?;
                *tool_rounds += 1;
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
                // Anti-recursion: delegation may not nest inside a delegated
                // sub-workflow.
                validate_delegated_step("skill_call", depth)?;
                let result =
                    Box::pin(self.execute_skill_at_depth(skill_id, input.clone(), ctx, depth + 1))
                        .await?;
                // Tool hygiene: strip tool-chatter command lines from
                // delegated text before it enters the parent context.
                let mut result = result;
                sanitize_result_value(&mut result);
                let result = self.review_nested_result(skill_id, input, result, ctx).await?;
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
                            sub_step,
                            ctx,
                            pool,
                            tenant_id,
                            run_id,
                            sub_index,
                            context,
                            depth,
                            tool_rounds,
                        ))
                        .await?;
                    }
                }
                Ok(())
            }
        }
    }

    /// Opt-in review gate for delegated (nested `SkillCall`) results.
    ///
    /// Off (`review_delegated_results == false`) or reviewer unavailable ⇒
    /// pass-through: the original result integrates unchanged. On rejection
    /// the result is replaced by a structured rejection carrying the review
    /// notes; the original stays intact under metadata for caller re-dispatch.
    async fn review_nested_result(
        &self,
        delegated_skill_id: &str,
        delegated_input: &serde_json::Value,
        result: serde_json::Value,
        ctx: &Arc<Context>,
    ) -> Result<serde_json::Value, String> {
        #[cfg(feature = "postgres")]
        {
            if !self.review_delegated_results {
                return Ok(result);
            }
            match
                review_delegated_result(ctx, delegated_skill_id, delegated_input, &result).await
            {
                Ok(v) if v.accepted => Ok(result),
                Ok(v) => Ok(serde_json::json!({
                    "status": "rejected",
                    "review": {
                        "accepted": false,
                        "notes": v.notes,
                    },
                    "metadata": {
                        "delegated_skill_id": delegated_skill_id,
                        "original_result": result,
                    },
                })),
                // Reviewer outage — degrade silently to pass-through.
                Err(_) => Ok(result),
            }
        }
        #[cfg(not(feature = "postgres"))]
        {
            // Review plumbing rides the LLM service; without postgres there is
            // no reviewer, so the gate is permanently off (pass-through).
            let _ = (self, delegated_skill_id, delegated_input, ctx);
            Ok(result)
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
///
/// `review_delegated_results` opts nested `SkillCall` results into a review
/// micro-step before they are integrated into the parent skill context.
/// Off by default; a missing or erroring reviewer degrades to pass-through.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SkillsPluginConfig {
    #[serde(default)]
    pub review_delegated_results: Option<bool>,
}

pub struct SkillsPlugin;

impl cordis::Plugin for SkillsPlugin {
    type Config = SkillsPluginConfig;
    type Provides = SkillsService;

    fn apply(
        &self,
        ctx: &Arc<Context>,
        config: Self::Config,
    ) -> Result<Arc<Self::Provides>, cordis::CordisError> {
        let execution = match ctx.get::<Execute>() {
            Some(e) => e,
            None => tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(ctx.inject::<Execute>())
            }),
        };
        let mut service = SkillsService::new(execution);
        service.review_delegated_results = config.review_delegated_results.unwrap_or(false);
        Ok(Arc::new(service))
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
            assert!(summaries[0].description.is_empty());
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

#[cfg(test)]
mod review_gate_tests {
    use super::{Execute, MAX_SKILL_CALL_DEPTH, SkillsService};
    use ares_llm::{ClientPool, Llm, ModelConfig, ProviderConfig, ProviderRegistry};
    use cordis::{Context, EventsService};
    use serde_json::json;
    use std::sync::Arc;

    /// Registry with an unreachable provider; the `llm.complete` waterfall
    /// handler short-circuits before any network call is attempted.
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

    fn service(gate_on: bool) -> SkillsService {
        SkillsService {
            execution: Arc::new(Execute::new()),
            max_depth: MAX_SKILL_CALL_DEPTH,
            review_delegated_results: gate_on,
        }
    }

    async fn ctx_with_reviewer(verdict_response: Option<&str>) -> Arc<Context> {
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());
        ctx.provide_arc(stub_llm());
        if let Some(response) = verdict_response {
            let body = response.to_string();
            events.on_waterfall("llm.complete".into(), move |_payload, _next| {
                let body = body.clone();
                async move { Ok(json!({ "content": body })) }
            });
        }
        ctx
    }

    #[tokio::test]
    async fn gate_off_passthrough_identical() {
        // Even a rejecting reviewer must not run while the gate is off.
        let ctx = ctx_with_reviewer(Some("REJECT inconsistent with input\nbad fit"))
            .await;
        let svc = service(false);
        let original = json!({"status":"success","answer":"42"});
        let out = svc
            .review_nested_result("child-skill", &json!({"q": "life"}), original.clone(), &ctx)
            .await
            .expect("pass-through");
        assert_eq!(out, original, "gate off must return the result unchanged");
    }

    #[tokio::test]
    async fn gate_on_accept_keeps_result() {
        let ctx = ctx_with_reviewer(Some("ACCEPT consistent and fits the task")).await;
        let svc = service(true);
        let original = json!({"status":"success","answer":"42"});
        let out = svc
            .review_nested_result("child-skill", &json!({"q": "life"}), original.clone(), &ctx)
            .await
            .expect("accepted result");
        assert_eq!(out, original, "accept keeps the original result verbatim");
    }

    #[tokio::test]
    async fn gate_on_reject_structures_rejection_with_notes() {
        let ctx = ctx_with_reviewer(Some(
            "REJECT answer does not match the requested quantity\nalso weak task fit",
        ))
        .await;
        let svc = service(true);
        let original = json!({"status":"success","answer":"banana"});
        let out = svc
            .review_nested_result("child-skill", &json!({"q": "apples"}), original.clone(), &ctx)
            .await
            .expect("structured rejection");
        assert_eq!(out.get("status"), Some(&json!("rejected")));
        let notes = out
            .pointer("/review/notes")
            .and_then(|v| v.as_str())
            .expect("review notes present");
        assert!(
            notes.contains("does not match") && notes.contains("task fit"),
            "notes must carry reviewer text, got {notes:?}"
        );
        assert_eq!(
            out.pointer("/metadata/delegated_skill_id"),
            Some(&json!("child-skill"))
        );
        assert_eq!(
            out.pointer("/metadata/original_result"),
            Some(&original),
            "original preserved for caller re-dispatch"
        );
    }

    #[tokio::test]
    async fn reviewer_error_passes_through() {
        // No Llm provided at all: reviewer outage degrades silently.
        let ctx = Context::new_root();
        let _events = ctx.provide(EventsService::new());
        let svc = service(true);
        let original = json!({"status":"success","answer":"42"});
        let out = svc
            .review_nested_result("child-skill", &json!({"q": "life"}), original.clone(), &ctx)
            .await
            .expect("outage pass-through");
        assert_eq!(out, original, "outage must not alter the result");
    }

    #[test]
    fn plugin_config_defaults_off_and_parses_toggle() {
        let cfg = <super::SkillsPluginConfig as Default>::default();
        assert_eq!(cfg.review_delegated_results, None);
        let parsed: super::SkillsPluginConfig =
            serde_json::from_str(r#"{"review_delegated_results": true}"#).unwrap();
        assert_eq!(parsed.review_delegated_results, Some(true));
    }
}

#[cfg(test)]
mod tool_hygiene_tests {
    use super::{
        check_tool_round_cap, sanitize_result_text, sanitize_result_value, validate_delegated_step,
        Execute, MAX_SKILL_CALL_DEPTH, SkillsService,
    };
    use ares_tools::{Tool, Tools};
    use cordis::{Context, EventsService};
    use serde_json::json;
    use sqlx::PgPool;
    use std::sync::Arc;

    /// Shared live-test pool; an unreachable DB fails loudly with fix hints.
    async fn try_test_pool() -> PgPool {
        ares_test_support::pool().await
    }

    /// Upsert one skill row with a fixed id (parent steps reference child ids).
    async fn seed_skill(pool: &PgPool, id: &str, tenant_id: &str, steps: serde_json::Value) {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO skills \
                (id, tenant_id, name, display_name, description, skill_type, steps, \
                 input_schema, output_schema, tools, is_public, created_by, created_at, updated_at) \
             VALUES ($1, $2, $3, $3, NULL, 'workflow', $4, NULL, NULL, NULL, FALSE, NULL, $5, $5) \
             ON CONFLICT (id) DO UPDATE SET tenant_id = EXCLUDED.tenant_id, \
                name = EXCLUDED.name, display_name = EXCLUDED.display_name, \
                steps = EXCLUDED.steps, updated_at = EXCLUDED.updated_at",
        )
        .bind(id)
        .bind(tenant_id)
        .bind(format!("hygiene-{id}"))
        .bind(steps)
        .bind(now)
        .execute(pool)
        .await
        .expect("seed skill row");
    }

    #[test]
    fn recursive_delegation_blocked_by_allowlist() {
        // The allowlist itself excludes delegation.
        assert!(
            !super::DELEGATED_SUB_TOOL_ALLOWLIST.contains(&"skill_call"),
            "skill_call must not be on the delegated sub-tool allowlist"
        );
        // Top-level execution may delegate freely.
        assert!(validate_delegated_step("skill_call", 0).is_ok());
        // Deeper executions refuse delegation with a stable marker.
        let err = validate_delegated_step("skill_call", 1).expect_err("delegation must be blocked");
        assert!(
            err.starts_with("delegated_step_not_allowed:"),
            "stable refusal marker expected, got {err:?}"
        );
        assert!(err.contains("skill_call"));
        // Allowlisted kinds still run at depth.
        for kind in super::DELEGATED_SUB_TOOL_ALLOWLIST {
            assert!(
                validate_delegated_step(kind, 3).is_ok(),
                "{kind} stays allowed at depth"
            );
        }
    }

    #[test]
    fn tool_round_cap_aborts_structured() {
        // Under the cap: fine (rounds already consumed).
        for rounds in 0..super::MAX_NESTED_TOOL_ROUNDS {
            assert!(check_tool_round_cap(rounds, "scope").is_ok());
        }
        // The round that would exceed the cap aborts with a stable prefix.
        let err = check_tool_round_cap(super::MAX_NESTED_TOOL_ROUNDS, "step 7")
            .expect_err("cap must abort");
        assert!(
            err.starts_with("tool_round_cap_exceeded:"),
            "structured abort marker expected, got {err:?}"
        );
        assert_eq!(super::MAX_NESTED_TOOL_ROUNDS, 3);
        assert_eq!(MAX_SKILL_CALL_DEPTH, super::MAX_NESTED_TOOL_ROUNDS + 5);
    }

    #[test]
    fn sanitize_strips_command_lines_from_result() {
        let text = "first line\n/run /tmp/scratch\n  /indented chatter\nresult tail";
        assert_eq!(
            sanitize_result_text(text),
            "first line\nresult tail",
            "leading-slash lines (any indent) must be stripped"
        );
        // Mid-line slashes stay untouched.
        assert_eq!(
            sanitize_result_text("path is /usr/bin/env — keep"),
            "path is /usr/bin/env — keep"
        );
        // Clean text returns unchanged (no allocation churn).
        assert_eq!(sanitize_result_text("clean"), "clean");
        // Nested string leaves are sanitized; other leaves survive verbatim.
        let mut value = json!({
            "status": "success",
            "content": "/reload config\nanswer: 42",
            "nested": {"deep": ["/ls -la", "kept", 7]},
            "count": 3
        });
        sanitize_result_value(&mut value);
        assert_eq!(value["content"], json!("answer: 42"));
        assert_eq!(value["nested"]["deep"][0], json!(""));
        assert_eq!(value["nested"]["deep"][1], json!("kept"));
        assert_eq!(value["nested"]["deep"][2], json!(7));
        assert_eq!(value["count"], json!(3));
    }

    /// Tool whose execution reports how many rounds ran this execution.
    struct CountingTool {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            "count"
        }
        fn description(&self) -> &str {
            "counts invocations"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> ares_types::Result<serde_json::Value> {
            Ok(json!({"n": self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst)}))
        }
    }

    struct ChatterTool;

    #[async_trait::async_trait]
    impl Tool for ChatterTool {
        fn name(&self) -> &str {
            "chatter"
        }
        fn description(&self) -> &str {
            "returns slash-command style chatter"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> ares_types::Result<serde_json::Value> {
            Ok(json!({"content": "/reload config\n/verbose on\nanswer: 42"}))
        }
    }

    fn hygiene_service(review_gate: bool) -> SkillsService {
        SkillsService {
            execution: Arc::new(Execute::new()),
            max_depth: MAX_SKILL_CALL_DEPTH,
            review_delegated_results: review_gate,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_round_cap_aborts_execution_after_three_tool_steps() {
        // block_in_place inside Tools::execute requires the multi-thread runtime.
        let pool = try_test_pool().await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let root = Context::new_root();
        root.provide(EventsService::new());
        root.provide(ares_store::PostgresClient {
            pool: pool.clone(),
        });
        // Skill steps resolve tools inside the tenant realm: provide into the
        // labeled isolate and pass THAT context onward (existing tool_call_tests pattern).
        let ctx = root.isolate::<Tools>("acme");
        ctx.provide(Tools::from_static([
            Arc::new(CountingTool {
                calls: Arc::clone(&calls),
            }) as Arc<dyn Tool>,
        ]));
        seed_skill(
            &pool,
            "hygiene-cap",
            "acme",
            json!([
                {"type": "tool_call", "tool_name": "count"},
                {"type": "tool_call", "tool_name": "count"},
                {"type": "tool_call", "tool_name": "count"},
                {"type": "tool_call", "tool_name": "count"}
            ]),
        )
        .await;
        let svc = hygiene_service(false);
        let err = svc
            .execute_skill(
                "hygiene-cap",
                json!({"tenant_id": "acme", "run_id": "cap-run"}),
                &ctx,
            )
            .await
            .expect_err("fourth tool round must abort");
        assert!(
            err.starts_with("tool_round_cap_exceeded:"),
            "structured cap error expected, got {err:?}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "exactly three tool rounds run before the abort"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sanitize_strips_command_lines_from_delegated_skill_result() {
        // block_in_place inside Tools::execute requires the multi-thread runtime.
        let pool = try_test_pool().await;
        let root = Context::new_root();
        root.provide(EventsService::new());
        root.provide(ares_store::PostgresClient {
            pool: pool.clone(),
        });
        let ctx = root.isolate::<Tools>("acme");
        ctx.provide(Tools::from_static([
            Arc::new(ChatterTool) as Arc<dyn Tool>,
        ]));
        // Parent delegates once (allowed at depth 0); the child runs one
        // chatty tool step. The delegated step's input carries the tenant so
        // the child resolves its own skill row; the integrated parent
        // context must carry clean text only.
        seed_skill(
            &pool,
            "hygiene-parent",
            "acme",
            json!([{
                "type": "skill_call",
                "skill_id": "hygiene-child",
                "input": {"tenant_id": "acme"}
            }]),
        )
        .await;
        seed_skill(
            &pool,
            "hygiene-child",
            "acme",
            json!([{"type": "tool_call", "tool_name": "chatter"}]),
        )
        .await;
        let svc = hygiene_service(true);
        let out = svc
            .execute_skill(
                "hygiene-parent",
                json!({"tenant_id": "acme", "run_id": "sanitize-run"}),
                &ctx,
            )
            .await
            .expect("parent skill executes");
        let content = out
            .pointer("/step_0/result/step_0/result/content")
            .and_then(|v| v.as_str())
            .expect("delegated tool content integrated under step_0");
        assert!(
            !content.contains("/reload"),
            "command chatter must not enter parent context, got {content:?}"
        );
        assert_eq!(content, "answer: 42");
    }

}
#[cfg(all(test, feature = "postgres"))]
mod delegation_flag_tests {
    use super::{parse_delegation_args, DelegationArgs, MAX_PARALLEL_DELEGATED_TASKS};

    #[test]
    fn parse_flags_quote_aware_tokens() {
        let parsed = parse_delegation_args(
            r#"translate "hello | world" --model fast-model --tools"#,
        )
        .expect("quoted delegation args should parse");
        assert_eq!(
            parsed.tasks,
            vec![vec![
                "translate".to_string(),
                "hello | world".to_string(),
            ]]
        );
        assert_eq!(parsed.model.as_deref(), Some("fast-model"));
        assert!(parsed.tools);
        assert!(!parsed.parallel);
    }

    #[test]
    fn parallel_latch_splits_tasks() {
        let parsed = parse_delegation_args(
            r#"alpha | beta --parallel gamma "delta echo" | epsilon"#,
        )
        .expect("latched delegation args should parse");
        assert!(parsed.parallel);
        assert_eq!(
            parsed.tasks,
            vec![
                vec!["alpha".to_string()],
                vec!["beta".to_string()],
                vec!["gamma".to_string()],
                vec!["delta echo".to_string()],
                vec!["epsilon".to_string()],
            ]
        );
    }

    #[test]
    fn model_flag_overrides_profile_default() {
        let flagged = parse_delegation_args("--model flag-model task").expect("parse");
        assert_eq!(
            DelegationArgs::resolved_model(&flagged, "profile-model", "global-model"),
            "flag-model"
        );

        let unflagged = parse_delegation_args("task").expect("parse");
        assert_eq!(
            DelegationArgs::resolved_model(&unflagged, "profile-model", "global-model"),
            "profile-model"
        );
        assert_eq!(
            DelegationArgs::resolved_model(&unflagged, "", "global-model"),
            "global-model"
        );
    }

    #[test]
    fn tools_flag_enables_inner_loop() {
        let off = parse_delegation_args("summarize notes").expect("parse");
        assert!(!off.tools, "--tools absent must keep the inner loop disabled");

        let on = parse_delegation_args("research topic --tools").expect("parse");
        assert!(on.tools, "--tools must enable the inner tool loop");
    }

    #[test]
    fn parallel_tasks_cap_at_configured_limit() {
        let names = (0..MAX_PARALLEL_DELEGATED_TASKS + 5)
            .map(|i| format!("t{i}"))
            .collect::<Vec<_>>();
        let parsed =
            parse_delegation_args(&format!("--parallel {}", names.join(" "))).expect("parse");
        assert!(parsed.parallel);
        assert_eq!(parsed.tasks.len(), MAX_PARALLEL_DELEGATED_TASKS);
    }
}
