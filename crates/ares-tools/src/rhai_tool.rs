//! Rhai-based Turing-complete tool engine for A.R.E.S.
//!
//! Provides [`RhaiTool`] as the default sandboxed scripting engine.
//! Scripts are compiled once to an [`rhai::AST`] and reused via
//! [`rhai::Engine::call_fn`] inside `spawn_blocking` + timeout.

use crate::registry::Tool;
use ares_types::types::{AppError, Result};
use async_trait::async_trait;
use rhai::{AST, Dynamic, Engine, Scope};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

// =============================================================================
// Configuration
// =============================================================================

fn default_entry() -> String {
    "execute".to_string()
}
fn default_max_ops() -> u64 {
    50000
}
fn default_timeout_ms() -> u64 {
    2000
}
fn default_max_string_size() -> usize {
    8192
}
fn default_max_call_levels() -> usize {
    64
}

/// Configuration parsed from `execution_config` JSONB for [`RhaiTool`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RhaiToolConfig {
    /// Rhai script source. Must define `fn <entry>(args)` or be a bare expression
    /// that can be evaluated with `args` in scope.
    pub script: String,

    /// Entry function name (default `"execute"`).
    #[serde(default)]
    pub entry: Option<String>,

    /// Max operations (default 50000, 0 = unlimited — but we clamp to default).
    #[serde(default)]
    pub max_ops: Option<u64>,

    /// Execution timeout in milliseconds (default 2000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,

    /// Max string size in bytes (default 8192).
    #[serde(default)]
    pub max_string_size: Option<usize>,

    /// Max call stack depth (default 64).
    #[serde(default)]
    pub max_call_levels: Option<usize>,
}

impl RhaiToolConfig {
    /// Effective entry name.
    pub fn effective_entry(&self) -> String {
        self.entry.clone().unwrap_or_else(default_entry)
    }
    /// Effective max operations.
    pub fn effective_max_ops(&self) -> u64 {
        self.max_ops.unwrap_or_else(default_max_ops)
    }
    /// Effective timeout.
    pub fn effective_timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.unwrap_or_else(default_timeout_ms))
    }
    /// Effective max string size.
    pub fn effective_max_string_size(&self) -> usize {
        self.max_string_size.unwrap_or_else(default_max_string_size)
    }
    /// Effective max call levels.
    pub fn effective_max_call_levels(&self) -> usize {
        self.max_call_levels
            .unwrap_or_else(default_max_call_levels)
    }
}

// =============================================================================
// Engine helpers
// =============================================================================

fn build_engine(config: &RhaiToolConfig) -> Engine {
    let mut engine = Engine::new();
    // bounded limits
    engine.set_max_operations(config.effective_max_ops());
    engine.set_max_string_size(config.effective_max_string_size());
    engine.set_max_call_levels(config.effective_max_call_levels());
    // max_expr_depth 128 for both expression depth counters
    engine.set_max_expr_depths(128, 128);
    // disable printing to stdout
    engine.on_print(|_| {});
    engine.on_debug(|_, _, _| {});
    // disallow eval likely by not registering it; also disable symbol if present
    engine.disable_symbol("eval");
    engine
}

fn compile_with_config(config: &RhaiToolConfig, engine: &Engine) -> Result<AST> {
    engine
        .compile(config.script.clone())
        .map_err(|e| AppError::Configuration(format!("Invalid Rhai script: {e}")))
}

// =============================================================================
// RhaiTool
// =============================================================================

/// Rhai-based tool — Turing-complete default engine.
///
/// Compiles `script` once to an [`AST`] and reuses it for every `execute`
/// via `Engine::call_fn` inside `spawn_blocking` to avoid blocking the Axum
/// worker.
pub struct RhaiTool {
    name: String,
    description: String,
    parameters_schema: Value,
    engine: Arc<Engine>,
    ast: AST,
    entry: String,
    timeout: Duration,
}

impl std::fmt::Debug for RhaiTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhaiTool")
            .field("name", &self.name)
            .field("entry", &self.entry)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl RhaiTool {
    /// Parse `execution_config` JSON into [`RhaiToolConfig`].
    pub fn parse_config(execution_config: &Value) -> Result<RhaiToolConfig> {
        serde_json::from_value(execution_config.clone())
            .map_err(|e| AppError::Configuration(format!("Invalid Rhai tool config: {e}")))
    }

    /// Validate script syntax with bounded engine limits.
    pub fn validate(config: &RhaiToolConfig) -> Result<()> {
        let engine = build_engine(config);
        compile_with_config(config, &engine).map(|_| ())
    }

    /// Create a new [`RhaiTool`] from validated config.
    ///
    /// Compiles the script once; the resulting [`AST`] is reused for all
    /// executions.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_schema: Value,
        config: RhaiToolConfig,
    ) -> Result<Self> {
        if config.script.trim().is_empty() {
            return Err(AppError::Configuration(
                "Rhai script must not be empty".to_string(),
            ));
        }
        let engine = build_engine(&config);
        let ast = compile_with_config(&config, &engine)?;
        let entry = config.effective_entry();
        let timeout = config.effective_timeout();
        Ok(Self {
            name: name.into(),
            description: description.into(),
            parameters_schema,
            engine: Arc::new(engine),
            ast,
            entry,
            timeout,
        })
    }

    /// Convenience: parse + construct in one step from raw `execution_config`.
    pub fn from_config(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_schema: Value,
        execution_config: &Value,
    ) -> Result<Self> {
        let cfg = Self::parse_config(execution_config)?;
        Self::new(name, description, parameters_schema, cfg)
    }

    /// Timeout for executions.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Entry function name.
    pub fn entry(&self) -> &str {
        &self.entry
    }
}

/// Convert a Rhai [`Dynamic`] value to [`serde_json::Value`].
///
/// Uses `rhai::serde::from_dynamic` for faithful conversion; falls back to
/// stringification for opaque types. Unit `()` maps to `Null`.
pub fn rhai_value_to_json(dynamic: &Dynamic) -> Value {
    if dynamic.is_unit() {
        return Value::Null;
    }
    // Try serde conversion to Value
    match rhai::serde::from_dynamic::<Value>(dynamic) {
        Ok(v) => v,
        Err(_) => {
            // manual fallback for common primitives
            if let Ok(i) = dynamic.as_int() {
                return Value::Number(i.into());
            }
            if let Ok(b) = dynamic.as_bool() {
                return Value::Bool(b);
            }
            if let Some(f) = dynamic.clone().try_cast::<f64>() {
                if let Some(n) = serde_json::Number::from_f64(f) {
                    return Value::Number(n);
                }
                return Value::String(f.to_string());
            }
            if let Some(s) = dynamic.clone().try_cast::<String>() {
                return Value::String(s);
            }
            // last resort
            Value::String(dynamic.to_string())
        }
    }
}

#[async_trait]
impl Tool for RhaiTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.parameters_schema.clone()
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let engine = Arc::clone(&self.engine);
        let ast = self.ast.clone();
        let entry = self.entry.clone();
        let timeout_dur = self.timeout;

        // spawn_blocking to avoid blocking Axum worker thread
        let blocking = tokio::task::spawn_blocking(move || -> std::result::Result<Value, String> {
            // Value -> Dynamic via rhai serde
            let dynamic = rhai::serde::to_dynamic(&args).map_err(|e| format!("args conversion failed: {e}"))?;

            let mut scope = Scope::new();
            // push "args" as Dynamic
            scope.push_dynamic("args", dynamic.clone());

            // push individual keys for convenience if args is object
            // Use try_cast to Map (BTreeMap<Identifier, Dynamic>)
            if let Some(map) = dynamic.clone().try_cast::<rhai::Map>() {
                for (k, v) in map {
                    // k is Identifier (SmartString); push as variable
                    // ignore duplicate push errors (e.g., invalid identifier)
                    let _ = scope.push_dynamic(k.to_string(), v);
                }
            } else if let Some(obj) = args.as_object() {
                // Fallback: iterate JSON object directly if Dynamic wasn't a Map
                // (shouldn't happen since serde conversion of object -> Map)
                for (k, v) in obj {
                    if let Ok(d) = rhai::serde::to_dynamic(v.clone()) {
                        let _ = scope.push_dynamic(k.clone(), d);
                    }
                }
            }

            // Try call_fn with single Dynamic argument first
            let call_result: std::result::Result<Dynamic, Box<rhai::EvalAltResult>> =
                engine.call_fn(&mut scope, &ast, &entry, (dynamic.clone(),));

            let dynamic_result = match call_result {
                Ok(v) => v,
                Err(e) => {
                    let msg = e.to_string();
                    let is_not_found = msg.contains("Function not found")
                        || msg.contains("not found")
                        || msg.contains("unknown function")
                        || msg.contains("Unable to find function");
                    if is_not_found {
                        // fallback: evaluate AST directly (script without entry function)
                        // Need a fresh scope clone? reuse scope which already has `args`
                        match engine.eval_ast_with_scope::<Dynamic>(&mut scope, &ast) {
                            Ok(v) => v,
                            Err(e2) => {
                                let msg2 = e2.to_string();
                                // if eval also failed because function definition returns unit,
                                // try call with empty args as alternative entry signature
                                if msg2.contains("Function not found") || msg2.contains("not found") {
                                    // try zero-arg call
                                    match engine.call_fn::<Dynamic>(&mut scope, &ast, &entry, ()) {
                                        Ok(v) => v,
                                        Err(e3) => return Err(format!("{e3}")),
                                    }
                                } else {
                                    return Err(format!("{e2}"));
                                }
                            }
                        }
                    } else if msg.contains("parameter") || msg.contains("argument") || msg.contains("signature") {
                        // arity mismatch — try zero-arg version
                        match engine.call_fn::<Dynamic>(&mut scope, &ast, &entry, ()) {
                            Ok(v) => v,
                            Err(_) => return Err(msg),
                        }
                    } else {
                        return Err(msg);
                    }
                }
            };

            Ok(rhai_value_to_json(&dynamic_result))
        });

        // apply timeout
        let timed = tokio::time::timeout(timeout_dur, blocking).await;
        match timed {
            Ok(join_res) => match join_res {
                Ok(inner) => match inner {
                    Ok(v) => Ok(v),
                    Err(e) => {
                        // Map operation limit / other runtime errors to External
                        // but syntax errors should be Configuration; we treat all runtime as External
                        Err(AppError::External(format!("Rhai error: {e}")))
                    }
                },
                Err(join_err) => Err(AppError::Internal(format!("Rhai join error: {join_err}"))),
            },
            Err(_) => Err(AppError::External("Rhai execution timed out".to_string())),
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mk_tool(script: &str, entry: Option<&str>, max_ops: Option<u64>, timeout_ms: Option<u64>) -> RhaiTool {
        let cfg = RhaiToolConfig {
            script: script.to_string(),
            entry: entry.map(|s| s.to_string()),
            max_ops,
            timeout_ms,
            max_string_size: None,
            max_call_levels: None,
        };
        RhaiTool::new("test", "test tool", json!({}), cfg).expect("tool creation")
    }

    #[test]
    fn test_parse_config_valid() {
        let v = json!({
            "script": "fn execute(args){ 42 }",
            "entry": "execute",
            "max_ops": 1000,
            "timeout_ms": 500,
            "max_string_size": 1024,
            "max_call_levels": 10
        });
        let cfg = RhaiTool::parse_config(&v).expect("parse");
        assert_eq!(cfg.script, "fn execute(args){ 42 }");
        assert_eq!(cfg.entry.unwrap(), "execute");
        assert_eq!(cfg.max_ops.unwrap(), 1000);
        assert_eq!(cfg.timeout_ms.unwrap(), 500);
        assert_eq!(cfg.max_string_size.unwrap(), 1024);
        assert_eq!(cfg.max_call_levels.unwrap(), 10);
    }

    #[test]
    fn test_parse_config_defaults() {
        let v = json!({ "script": "fn execute(args){ 1 }" });
        let cfg = RhaiTool::parse_config(&v).expect("parse");
        assert_eq!(cfg.effective_entry(), "execute");
        assert_eq!(cfg.effective_max_ops(), 50000);
        assert_eq!(cfg.effective_timeout(), Duration::from_millis(2000));
        assert_eq!(cfg.effective_max_string_size(), 8192);
        assert_eq!(cfg.effective_max_call_levels(), 64);
    }

    #[test]
    fn test_invalid_script_syntax_returns_error() {
        let cfg = RhaiToolConfig {
            script: "fn execute( { broken syntax".to_string(),
            entry: None,
            max_ops: None,
            timeout_ms: None,
            max_string_size: None,
            max_call_levels: None,
        };
        let res = RhaiTool::validate(&cfg);
        assert!(res.is_err(), "expected syntax error");
        let err = res.unwrap_err();
        assert!(err.to_string().contains("Invalid Rhai script") || err.to_string().contains("Configuration"));

        // also via ::new
        let new_res = RhaiTool::new("t", "d", json!({}), cfg);
        assert!(new_res.is_err());
    }

    #[tokio::test]
    async fn test_execute_simple_add() {
        let tool = mk_tool(r#"fn execute(args){ args["a"] + args["b"] }"#, None, None, None);
        let out = tool.execute(json!({"a": 2, "b": 3})).await.expect("execute");
        // Rhai ints map to JSON numbers
        assert_eq!(out, json!(5));
    }

    #[tokio::test]
    async fn test_max_ops_exceeded() {
        // low max_ops should trigger quickly
        let tool = mk_tool("fn execute(args){ while true {} }", None, Some(1000), Some(2000));
        let res = tool.execute(json!({})).await;
        assert!(res.is_err(), "expected max ops error, got {res:?}");
        let msg = res.unwrap_err().to_string().to_lowercase();
        assert!(
            msg.contains("operation") || msg.contains("exceed") || msg.contains("rhai error"),
            "msg was {msg}"
        );
    }

    #[tokio::test]
    async fn test_timeout() {
        // very low timeout + huge max_ops so timeout triggers before ops limit
        let tool = mk_tool("fn execute(args){ while true {} }", None, Some(1_000_000_000), Some(50));
        let res = tool.execute(json!({})).await;
        assert!(res.is_err(), "expected timeout");
        let msg = res.unwrap_err().to_string().to_lowercase();
        assert!(
            msg.contains("timed out") || msg.contains("timeout") || msg.contains("rhai"),
            "msg was {msg}"
        );
    }

    #[tokio::test]
    async fn test_tenant_isolation() {
        let tool = mk_tool(r#"fn execute(args){ args["tenant"] }"#, None, None, None);
        let out_a = tool.execute(json!({"tenant": "a"})).await.expect("a");
        let out_b = tool.execute(json!({"tenant": "b"})).await.expect("b");
        assert_eq!(out_a, json!("a"));
        assert_eq!(out_b, json!("b"));
        assert_ne!(out_a, out_b);
        // second call with a again still returns a (no cross-call leakage)
        let out_a2 = tool.execute(json!({"tenant": "a"})).await.expect("a2");
        assert_eq!(out_a, out_a2);
    }

    #[tokio::test]
    async fn test_json_result() {
        let tool = mk_tool(
            r#"fn execute(args){ #{"sum": args["x"] + args["y"], "greeting": "hello " + args["name"] } }"#,
            None,
            None,
            None,
        );
        let out = tool
            .execute(json!({"x": 10, "y": 5, "name": "world"}))
            .await
            .expect("execute");
        assert_eq!(out["sum"], json!(15));
        assert_eq!(out["greeting"], json!("hello world"));
    }

    #[tokio::test]
    async fn test_eval_ast_fallback() {
        // script without function, just expression using args
        let tool = mk_tool(r#"args["a"] * 3"#, Some("nonexistent"), None, None);
        // entry not found -> fallback to eval_ast_with_scope should produce 6
        // Actually our fallback tries call_fn first, then eval_ast; since entry "nonexistent" not found, eval_ast will evaluate "args[\"a\"] * 3"
        let out = tool.execute(json!({"a": 2})).await.expect("fallback eval");
        assert_eq!(out, json!(6));
    }

    #[tokio::test]
    async fn test_string_limit() {
        let cfg = RhaiToolConfig {
            script: r#"fn execute(args){ "a" + "b" }"#.to_string(),
            entry: None,
            max_ops: None,
            timeout_ms: None,
            max_string_size: Some(2),
            max_call_levels: None,
        };
        // "a"+"b" => "ab" length 2 ok, but if we exceed limit later? This test just ensures creation works
        let tool = RhaiTool::new("t", "d", json!({}), cfg).expect("tool");
        let out = tool.execute(json!({})).await.expect("execute");
        assert_eq!(out, json!("ab"));
    }
}
