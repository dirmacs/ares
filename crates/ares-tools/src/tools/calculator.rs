use crate::registry::Tool;
use ares_types::Result;
use async_trait::async_trait;
use cordis::Service;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// JSON schema shared by `Calculator` and `CalculatorService`.
fn calculator_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operation": {
                "type": "string",
                "enum": ["add", "subtract", "multiply", "divide"]
            },
            "a": { "type": "number" },
            "b": { "type": "number" }
        },
        "required": ["operation", "a", "b"]
    })
}

/// Perform the arithmetic described by `args`, or fail with
/// [`AppError::InvalidInput`](ares_types::AppError::InvalidInput) when the input
/// is missing, non-numeric, names an unsupported operation, or divides by zero.
///
/// Every fallback that used to fabricate a plausible-looking result (a missing
/// operand becoming `0.0`, an unknown operation becoming `0.0`, division by
/// zero serializing as `null`) now surfaces as an error the caller can react to.
fn calculate(args: &Value) -> Result<Value> {
    let op = args["operation"]
        .as_str()
        .ok_or_else(|| ares_types::AppError::InvalidInput("operation is required".to_string()))?;

    let a = args["a"]
        .as_f64()
        .ok_or_else(|| ares_types::AppError::InvalidInput("a must be a number".to_string()))?;
    let b = args["b"]
        .as_f64()
        .ok_or_else(|| ares_types::AppError::InvalidInput("b must be a number".to_string()))?;

    let result = match op {
        "add" => a + b,
        "subtract" => a - b,
        "multiply" => a * b,
        "divide" => {
            if b == 0.0 {
                return Err(ares_types::AppError::InvalidInput(
                    "division by zero".to_string(),
                ));
            }
            a / b
        }
        _ => {
            return Err(ares_types::AppError::InvalidInput(format!(
                "unsupported operation: {op}"
            )));
        }
    };

    Ok(json!({ "result": result }))
}

/// Calculator tool for basic arithmetic operations.
pub struct Calculator;

#[async_trait]
impl Tool for Calculator {
    fn name(&self) -> &str {
        "calculator"
    }

    fn description(&self) -> &str {
        "Perform basic arithmetic operations"
    }

    fn parameters_schema(&self) -> Value {
        calculator_schema()
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        calculate(&args)
    }
}

// Cordis Service wrapper, used for dependency injection via Context.

/// Empty config for CalculatorService. Default suffices; Serialize and
/// Deserialize are required for Plugin Config via RegistryService.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalculatorConfig;

/// CalculatorService wraps Calculator for Cordis dependency injection.
///
/// Service provides the feature gate boundary. Tool implements the same
/// arithmetic logic as Calculator without any behavior change.
pub struct CalculatorService;

impl CalculatorService {
    /// Create with default config.
    pub fn new() -> Self {
        Self
    }

    /// Create from explicit config. Config is currently empty and kept
    /// for Plugin compatibility.
    pub fn with_config(_config: CalculatorConfig) -> Self {
        Self
    }
}

impl Default for CalculatorService {
    fn default() -> Self {
        Self::new()
    }
}

impl Service for CalculatorService {
    fn name(&self) -> &'static str {
        "calculator"
    }

    fn init(&self, _ctx: &std::sync::Arc<cordis::Context>) -> cordis::ServiceInitFuture<'_> {
        Box::pin(async move { Ok(None) })
    }
}

#[async_trait]
impl Tool for CalculatorService {
    fn name(&self) -> &str {
        "calculator"
    }

    fn description(&self) -> &str {
        "Perform basic arithmetic operations"
    }

    fn parameters_schema(&self) -> Value {
        calculator_schema()
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        calculate(&args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_name_and_description() {
        let tool = Calculator;
        assert_eq!(tool.name(), "calculator");
        assert_eq!(tool.description(), "Perform basic arithmetic operations");
    }

    #[test]
    fn test_parameters_schema() {
        let tool = Calculator;
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(
            schema["properties"]["operation"]["enum"],
            json!(["add", "subtract", "multiply", "divide"])
        );
        assert_eq!(schema["required"], json!(["operation", "a", "b"]));
    }

    #[tokio::test]
    async fn test_add() {
        let tool = Calculator;
        let out = tool
            .execute(json!({"operation": "add", "a": 10.0, "b": 3.5}))
            .await
            .unwrap();
        assert_eq!(out["result"], json!(13.5));
    }

    #[tokio::test]
    async fn test_subtract() {
        let tool = Calculator;
        let out = tool
            .execute(json!({"operation": "subtract", "a": 10.0, "b": 3.0}))
            .await
            .unwrap();
        assert_eq!(out["result"], json!(7.0));
    }

    #[tokio::test]
    async fn test_multiply() {
        let tool = Calculator;
        let out = tool
            .execute(json!({"operation": "multiply", "a": 4.0, "b": 2.5}))
            .await
            .unwrap();
        assert_eq!(out["result"], json!(10.0));
    }

    #[tokio::test]
    async fn test_divide() {
        let tool = Calculator;
        let out = tool
            .execute(json!({"operation": "divide", "a": 10.0, "b": 4.0}))
            .await
            .unwrap();
        assert_eq!(out["result"], json!(2.5));
    }

    #[tokio::test]
    async fn test_unknown_operation_errors() {
        let tool = Calculator;
        let err = tool
            .execute(json!({"operation": "modulo", "a": 10.0, "b": 3.0}))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::InvalidInput(msg) if msg.contains("unsupported operation: modulo")
        ));
    }

    #[tokio::test]
    async fn test_missing_operation_errors() {
        let tool = Calculator;
        let err = tool.execute(json!({"a": 2.0, "b": 3.0})).await.unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::InvalidInput(msg) if msg.contains("operation is required")
        ));
    }

    #[tokio::test]
    async fn test_missing_operands_error() {
        let tool = Calculator;
        let err = tool.execute(json!({"operation": "add"})).await.unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::InvalidInput(msg) if msg.contains("a must be a number")
        ));
    }

    #[tokio::test]
    async fn test_divide_by_zero_errors() {
        let tool = Calculator;
        let err = tool
            .execute(json!({"operation": "divide", "a": 1.0, "b": 0.0}))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::InvalidInput(msg) if msg.contains("division by zero")
        ));
    }

    #[tokio::test]
    async fn test_non_numeric_operand_errors() {
        let tool = Calculator;
        let err = tool
            .execute(json!({"operation": "add", "a": "not-a-number", "b": 3.0}))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::InvalidInput(msg) if msg.contains("a must be a number")
        ));
    }

    #[tokio::test]
    async fn test_calculator_service_reports_bad_input() {
        let tool = CalculatorService;
        let err = tool
            .execute(json!({"operation": "modulo", "a": 10.0, "b": 3.0}))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::InvalidInput(msg) if msg.contains("unsupported operation: modulo")
        ));
    }

    // Verify Calculator is resolved through Tools on Context.

    #[test]
    fn test_calculator_service_via_context() {
        use crate::Tools;
        use crate::registry::ToolRegistry;
        use cordis::Context;
        use std::sync::Arc;

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CalculatorService));
        let tools = Arc::new(Tools::new(Arc::new(registry)));
        let ctx = Context::new_root();
        ctx.provide_arc(Arc::clone(&tools));

        let resolved = ctx.get::<Tools>().expect("Tools should be in context");
        let tool = resolved
            .resolve(&ctx, "calculator")
            .expect("calculator should resolve");
        assert_eq!(tool.name(), "calculator");

        let list = resolved.list(&ctx);
        assert!(list.iter().any(|d| d.name == "calculator"));
        assert!(resolved.resolve(&ctx, "unknown").is_none());

        let isolated = ctx.isolate::<Tools>("tenant:acme");
        assert!(resolved.resolve(&isolated, "calculator").is_some());
        assert!(
            resolved
                .list(&isolated)
                .iter()
                .any(|d| d.name == "calculator")
        );
    }

    #[test]
    fn test_calculator_via_tools_list_resolve() {
        use crate::Tools;
        use crate::registry::ToolRegistry;
        use cordis::Context;
        use std::sync::Arc;

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CalculatorService));
        let svc = Tools::new(Arc::new(registry));
        let ctx = Context::new_root().isolate::<Tools>("tenant:acme");
        let tool = svc.resolve(&ctx, "calculator").unwrap();
        assert_eq!(tool.name(), "calculator");
        assert_eq!(tool.description(), "Perform basic arithmetic operations");
        let defs = svc.list(&ctx);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "calculator");
    }
}
