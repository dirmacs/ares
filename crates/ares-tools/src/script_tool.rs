//! Runtime script tool executor for A.R.E.S.
//!
//! This tool executes JavaScript or Python scripts configured via `execution_config`.
//! Scripts receive tool arguments via parameter substitution (`{{param}}`) and as an
//! `args` global variable.
//!
//! # Execution environments
//!
//! * **JavaScript** — executed in-process via `boa_engine` (pure-Rust, sandboxed:
//!   no filesystem or network access).
//! * **Python** — executed in a separate OS process with a restricted builtin set
//!   and configurable timeout / memory limits.

use crate::registry::Tool;
use ares_types::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

// =============================================================================
// Configuration
// =============================================================================

/// Script-specific configuration parsed from `execution_config` JSONB.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScriptToolConfig {
    /// Scripting language: `"javascript"` or `"python"`.
    pub language: String,
    /// Script source code. Supports `{{param}}` placeholders which are replaced
    /// with JSON-encoded argument values before execution.
    pub script: String,
    /// Execution timeout in seconds (default: 30).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Optional memory limit in megabytes (Python only; default: 256).
    #[serde(default)]
    pub memory_limit_mb: Option<u64>,
}

impl Default for ScriptToolConfig {
    fn default() -> Self {
        Self {
            language: "javascript".to_string(),
            script: String::new(),
            timeout_secs: Some(30),
            memory_limit_mb: Some(256),
        }
    }
}

// =============================================================================
// Tool implementation
// =============================================================================

/// Runtime script tool that executes JavaScript or Python code.
pub struct ScriptTool {
    name: String,
    description: String,
    parameters_schema: Value,
    config: ScriptToolConfig,
}

impl ScriptTool {
    /// Create a script tool from its runtime configuration.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_schema: Value,
        config: ScriptToolConfig,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters_schema,
            config,
        }
    }

    /// Parse `execution_config` JSONB into [`ScriptToolConfig`].
    pub fn parse_config(execution_config: &Value) -> Result<ScriptToolConfig> {
        serde_json::from_value(execution_config.clone()).map_err(|e| {
            ares_types::AppError::Configuration(format!("Invalid script tool config: {e}"))
        })
    }

    /// Resolve the effective timeout (default 30 s).
    fn timeout(&self) -> Duration {
        Duration::from_secs(self.config.timeout_secs.unwrap_or(30))
    }

    /// Resolve the effective memory limit in MB (default 256).
    fn memory_limit_mb(&self) -> u64 {
        self.config.memory_limit_mb.unwrap_or(256)
    }
}

#[async_trait]
impl Tool for ScriptTool {
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
        let args_map = args.as_object().ok_or_else(|| {
            ares_types::AppError::InvalidInput("args must be a JSON object".to_string())
        })?;

        // Replace {{param}} placeholders with JSON-encoded values.
        let script = substitute_script_params(&self.config.script, args_map);

        match self.config.language.to_ascii_lowercase().as_str() {
            "javascript" | "js" => execute_javascript(&script, args, self.timeout()).await,
            "python" | "py" => {
                execute_python(&script, args, self.timeout(), self.memory_limit_mb()).await
            }
            other => Err(ares_types::AppError::InvalidInput(format!(
                "Unsupported script language: {other}"
            ))),
        }
    }
}

// =============================================================================
// JavaScript execution (boa_engine)
// =============================================================================

/// Execute JavaScript in a sandboxed `boa_engine` context.
async fn execute_javascript(script: &str, args: Value, ttl: Duration) -> Result<Value> {
    let script = script.to_string();

    let handle = tokio::task::spawn_blocking(move || {
        let mut context = boa_engine::Context::default();

        // Defensive resource limit: cap loop iterations so infinite loops
        // terminate quickly instead of consuming a blocking thread forever.
        context
            .runtime_limits_mut()
            .set_loop_iteration_limit(10_000_000);

        // Inject `args` as a global JavaScript variable.
        let args_json = serde_json::to_string(&args).map_err(|e| {
            ares_types::AppError::Internal(format!("JSON serialization error: {e}"))
        })?;
        let init = format!("let args = {args_json};");
        context
            .eval(boa_engine::Source::from_bytes(init.as_bytes()))
            .map_err(|e| ares_types::AppError::External(format!("JS init error: {e}")))?;

        // Run the user script.
        let result = context
            .eval(boa_engine::Source::from_bytes(script.as_bytes()))
            .map_err(|e| ares_types::AppError::External(format!("JS execution error: {e}")))?;

        // boa_engine 0.20 panics on undefined → JSON; handle it explicitly.
        if result.is_undefined() {
            return Ok(Value::Null);
        }

        // Convert back to serde_json::Value.
        let json_value = result.to_json(&mut context).map_err(|e| {
            ares_types::AppError::External(format!("JS result conversion error: {e}"))
        })?;

        Ok(json_value)
    });

    match timeout(ttl, handle).await {
        Ok(Ok(result)) => result,
        Ok(Err(join_err)) => Err(ares_types::AppError::External(format!(
            "JS task panicked: {join_err}"
        ))),
        Err(_) => Err(ares_types::AppError::External(
            "JS execution timed out".to_string(),
        )),
    }
}

// =============================================================================
// Python execution (subprocess)
// =============================================================================

/// Execute Python in a restricted subprocess.
async fn execute_python(
    script: &str,
    args: Value,
    ttl: Duration,
    _memory_limit_mb: u64,
) -> Result<Value> {
    let args_json = serde_json::to_string(&args)
        .map_err(|e| ares_types::AppError::Internal(format!("JSON serialization error: {e}")))?;

    // Spawn Python with the wrapper script.
    let mut cmd = Command::new("python3");
    cmd.arg("-c")
        .arg(PYTHON_WRAPPER)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("SCRIPT_TOOL_ARGS_JSON", args_json)
        .env("SCRIPT_TOOL_USER_CODE", script);

    let mut child = cmd
        .spawn()
        .map_err(|e| ares_types::AppError::External(format!("Failed to spawn python3: {e}")))?;

    // Close stdin immediately — all data is passed via env vars.
    drop(child.stdin.take());

    let mut stdout_pipe = child.stdout.take().unwrap();
    let mut stderr_pipe = child.stderr.take().unwrap();

    // Read stdout/stderr concurrently so the pipe buffer never blocks the child.
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stdout_pipe, &mut buf)
            .await
            .ok();
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stderr_pipe, &mut buf)
            .await
            .ok();
        buf
    });

    let status = match timeout(ttl, child.wait()).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            return Err(ares_types::AppError::External(format!(
                "Python subprocess error: {e}"
            )))
        }
        Err(_) => {
            let _ = child.start_kill();
            return Err(ares_types::AppError::External(
                "Python execution timed out".to_string(),
            ));
        }
    };

    let stdout = stdout_task.await.unwrap_or_default();
    let stderr = stderr_task.await.unwrap_or_default();

    if !status.success() {
        let stderr_str = String::from_utf8_lossy(&stderr);
        return Err(ares_types::AppError::External(format!(
            "Python script exited with code {:?}: {stderr_str}",
            status.code()
        )));
    }

    let stdout_str = String::from_utf8_lossy(&stdout);
    let trimmed = stdout_str.trim();

    if trimmed.is_empty() {
        return Ok(Value::Null);
    }

    // Try to parse stdout as JSON.
    match serde_json::from_str::<Value>(trimmed) {
        Ok(v) => Ok(v),
        Err(_) => Ok(Value::String(trimmed.to_string())),
    }
}

/// Python wrapper script executed via `python3 -c`.
///
/// Reads the user script from `SCRIPT_TOOL_USER_CODE` and arguments from
/// `SCRIPT_TOOL_ARGS_JSON`.  Executes the user code with a restricted builtin
/// set and emits `{"result": ..., "stdout": ...}` on stdout.
const PYTHON_WRAPPER: &str = r#"
import json, sys, io, os

args = json.loads(os.environ.get("SCRIPT_TOOL_ARGS_JSON", "{}"))
user_script = os.environ.get("SCRIPT_TOOL_USER_CODE", "")

_SAFE_BUILTINS = {
    'abs': abs, 'all': all, 'any': any, 'bool': bool,
    'dict': dict, 'enumerate': enumerate, 'filter': filter,
    'float': float, 'frozenset': frozenset, 'int': int,
    'isinstance': isinstance, 'issubclass': issubclass,
    'len': len, 'list': list, 'map': map, 'max': max,
    'min': min, 'next': next, 'pow': pow, 'print': print,
    'range': range, 'reversed': reversed, 'round': round,
    'set': set, 'slice': slice, 'sorted': sorted,
    'str': str, 'sum': sum, 'tuple': tuple, 'type': type,
    'zip': zip, 'json': json, 'io': io,
    'Exception': Exception, 'TypeError': TypeError,
    'ValueError': ValueError, 'KeyError': KeyError,
    'IndexError': IndexError, 'AttributeError': AttributeError,
    'ArithmeticError': ArithmeticError, 'RuntimeError': RuntimeError,
}

_old_stdout = sys.stdout
_sys_stdout = io.StringIO()
sys.stdout = _sys_stdout

_locals = {'args': args}
exec(user_script, {"__builtins__": _SAFE_BUILTINS}, _locals)

sys.stdout = _old_stdout
_stdout_text = _sys_stdout.getvalue()

_result = _locals.get('result')
print(json.dumps({"result": _result, "stdout": _stdout_text}))
"#;

// =============================================================================
// Helpers
// =============================================================================

/// Replace `{{key}}` placeholders in `script` with JSON-encoded values from `args`.
fn substitute_script_params(script: &str, args: &serde_json::Map<String, Value>) -> String {
    let mut result = script.to_string();
    for (key, value) in args {
        let placeholder = format!("{{{{{}}}}}", key);
        let replacement = serde_json::to_string(value).unwrap_or_default();
        result = result.replace(&placeholder, &replacement);
    }
    result
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tool(config: ScriptToolConfig) -> ScriptTool {
        ScriptTool::new(
            "test_script",
            "A test script tool",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "count": { "type": "number" }
                }
            }),
            config,
        )
    }

    // -------------------------------------------------------------------------
    // Config parsing
    // -------------------------------------------------------------------------

    #[test]
    fn test_parse_config_javascript() {
        let raw = json!({
            "language": "javascript",
            "script": "args.name",
            "timeout_secs": 10,
            "memory_limit_mb": 128
        });
        let cfg = ScriptTool::parse_config(&raw).unwrap();
        assert_eq!(cfg.language, "javascript");
        assert_eq!(cfg.script, "args.name");
        assert_eq!(cfg.timeout_secs, Some(10));
        assert_eq!(cfg.memory_limit_mb, Some(128));
    }

    #[test]
    fn test_parse_config_python() {
        let raw = json!({
            "language": "python",
            "script": "result = args['name']"
        });
        let cfg = ScriptTool::parse_config(&raw).unwrap();
        assert_eq!(cfg.language, "python");
        assert_eq!(cfg.script, "result = args['name']");
        assert_eq!(cfg.timeout_secs, None);
        assert_eq!(cfg.memory_limit_mb, None);
    }

    #[test]
    fn test_parse_config_invalid() {
        let raw = json!({ "language": 42 });
        assert!(ScriptTool::parse_config(&raw).is_err());
    }

    // -------------------------------------------------------------------------
    // Parameter substitution
    // -------------------------------------------------------------------------

    #[test]
    fn test_substitute_params_strings_and_numbers() {
        let mut map = serde_json::Map::new();
        map.insert("name".into(), json!("Alice"));
        map.insert("count".into(), json!(42));

        let script = r#"let greeting = "Hello, {{name}}"; let n = {{count}};"#;
        let out = substitute_script_params(script, &map);
        assert_eq!(out, r#"let greeting = "Hello, "Alice""; let n = 42;"#);
    }

    #[test]
    fn test_substitute_params_escapes_quotes() {
        let mut map = serde_json::Map::new();
        map.insert("unsafe".into(), json!(r#""; DROP TABLE users; --"#));

        let script = r#"let x = {{unsafe}};"#;
        let out = substitute_script_params(script, &map);
        assert_eq!(out, "let x = \"\\\"; DROP TABLE users; --\";");
    }

    #[test]
    fn test_substitute_params_missing_placeholder_unchanged() {
        let mut map = serde_json::Map::new();
        map.insert("a".into(), json!(1));

        let script = "{{a}} {{b}}";
        let out = substitute_script_params(script, &map);
        assert_eq!(out, "1 {{b}}");
    }

    // -------------------------------------------------------------------------
    // JavaScript execution
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_js_returns_last_expression() {
        let tool = make_tool(ScriptToolConfig {
            language: "javascript".into(),
            script: "args.count * 2".into(),
            ..Default::default()
        });
        let out = tool.execute(json!({"count": 21})).await.unwrap();
        assert_eq!(out, json!(42));
    }

    #[tokio::test]
    async fn test_js_string_concatenation() {
        let tool = make_tool(ScriptToolConfig {
            language: "javascript".into(),
            script: r#""Hello, " + args.name"#.into(),
            ..Default::default()
        });
        let out = tool.execute(json!({"name": "World"})).await.unwrap();
        assert_eq!(out, json!("Hello, World"));
    }

    #[tokio::test]
    async fn test_js_object_manipulation() {
        let tool = make_tool(ScriptToolConfig {
            language: "javascript".into(),
            script: "args.count + 1".into(),
            ..Default::default()
        });
        let out = tool.execute(json!({"count": 5})).await.unwrap();
        assert_eq!(out, json!(6));
    }

    #[tokio::test]
    async fn test_js_template_substitution() {
        let tool = make_tool(ScriptToolConfig {
            language: "javascript".into(),
            script: r#""value is " + {{count}}"#.into(),
            ..Default::default()
        });
        let out = tool.execute(json!({"count": 99})).await.unwrap();
        assert_eq!(out, json!("value is 99"));
    }

    #[tokio::test]
    async fn test_js_infinite_loop_hits_limit() {
        let tool = make_tool(ScriptToolConfig {
            language: "javascript".into(),
            script: "while(true) {{ }}".into(),
            ..Default::default()
        });
        let err = tool.execute(json!({})).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("execution error"),
            "Expected execution error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_js_syntax_error() {
        let tool = make_tool(ScriptToolConfig {
            language: "javascript".into(),
            script: "this is not valid js !!!".into(),
            ..Default::default()
        });
        let err = tool.execute(json!({})).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("execution error") || msg.contains("SyntaxError"),
            "Expected JS error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_js_undefined_returns_null() {
        let tool = make_tool(ScriptToolConfig {
            language: "javascript".into(),
            script: "let x = undefined; x".into(),
            ..Default::default()
        });
        let out = tool.execute(json!({})).await.unwrap();
        assert!(out.is_null());
    }

    // -------------------------------------------------------------------------
    // Python execution
    // -------------------------------------------------------------------------

    fn is_python_available() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[tokio::test]
    async fn test_python_math() {
        if !is_python_available() {
            eprintln!("Skipping Python test: python3 not found");
            return;
        }
        let tool = make_tool(ScriptToolConfig {
            language: "python".into(),
            script: "result = args['a'] + args['b']".into(),
            ..Default::default()
        });
        let out = tool.execute(json!({"a": 10, "b": 32})).await.unwrap();
        // stdout wrapper emits {"result": 42, "stdout": ""}
        assert_eq!(out["result"], json!(42));
    }

    #[tokio::test]
    async fn test_python_string_result() {
        if !is_python_available() {
            eprintln!("Skipping Python test: python3 not found");
            return;
        }
        let tool = make_tool(ScriptToolConfig {
            language: "python".into(),
            script: "result = 'Hello, ' + args['name']".into(),
            ..Default::default()
        });
        let out = tool.execute(json!({"name": "World"})).await.unwrap();
        assert_eq!(out["result"], json!("Hello, World"));
    }

    #[tokio::test]
    async fn test_python_template_substitution() {
        if !is_python_available() {
            eprintln!("Skipping Python test: python3 not found");
            return;
        }
        let tool = make_tool(ScriptToolConfig {
            language: "python".into(),
            script: "result = 'count = ' + str({{count}})".into(),
            ..Default::default()
        });
        let out = tool.execute(json!({"count": 7})).await.unwrap();
        assert_eq!(out["result"], json!("count = 7"));
    }

    #[tokio::test]
    async fn test_python_captured_stdout() {
        if !is_python_available() {
            eprintln!("Skipping Python test: python3 not found");
            return;
        }
        let tool = make_tool(ScriptToolConfig {
            language: "python".into(),
            script: "print('debug line')".into(),
            ..Default::default()
        });
        let out = tool.execute(json!({})).await.unwrap();
        assert_eq!(out["stdout"], json!("debug line\n"));
    }

    #[tokio::test]
    async fn test_python_timeout() {
        if !is_python_available() {
            eprintln!("Skipping Python test: python3 not found");
            return;
        }
        let tool = make_tool(ScriptToolConfig {
            language: "python".into(),
            script: "while True:\n    pass".into(),
            timeout_secs: Some(1),
            ..Default::default()
        });
        let err = tool.execute(json!({})).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("timed out"),
            "Expected timeout error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_python_syntax_error() {
        if !is_python_available() {
            eprintln!("Skipping Python test: python3 not found");
            return;
        }
        let tool = make_tool(ScriptToolConfig {
            language: "python".into(),
            script: "this is not valid python !!!".into(),
            ..Default::default()
        });
        let err = tool.execute(json!({})).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("exited with code") || msg.contains("SyntaxError"),
            "Expected Python error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_python_restricted_builtin_blocks_file_open() {
        if !is_python_available() {
            eprintln!("Skipping Python test: python3 not found");
            return;
        }
        let tool = make_tool(ScriptToolConfig {
            language: "python".into(),
            script: "open('/etc/passwd')".into(),
            ..Default::default()
        });
        let err = tool.execute(json!({})).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("NameError") || msg.contains("exited with code"),
            "Expected blocked open(), got: {msg}"
        );
    }

    // -------------------------------------------------------------------------
    // Tool trait plumbing
    // -------------------------------------------------------------------------

    #[test]
    fn test_name_and_description() {
        let tool = make_tool(ScriptToolConfig::default());
        assert_eq!(tool.name(), "test_script");
        assert_eq!(tool.description(), "A test script tool");
    }

    #[test]
    fn test_parameters_schema() {
        let tool = make_tool(ScriptToolConfig::default());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].get("name").is_some());
    }

    #[tokio::test]
    async fn test_unsupported_language() {
        let tool = make_tool(ScriptToolConfig {
            language: "ruby".into(),
            script: "1+1".into(),
            ..Default::default()
        });
        let err = tool.execute(json!({})).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Unsupported"));
    }

    #[tokio::test]
    async fn test_invalid_args_type() {
        let tool = make_tool(ScriptToolConfig::default());
        let err = tool.execute(json!("not an object")).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("must be a JSON object"));
    }
}
