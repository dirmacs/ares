use crate::registry::Tool;
use ares_types::Result;
use async_trait::async_trait;
use cordis::Service;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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

    async fn execute(&self, args: Value) -> Result<Value> {
        let op = args["operation"].as_str().unwrap_or("add");
        let a = args["a"].as_f64().unwrap_or(0.0);
        let b = args["b"].as_f64().unwrap_or(0.0);

        let result = match op {
            "add" => a + b,
            "subtract" => a - b,
            "multiply" => a * b,
            "divide" => a / b,
            _ => 0.0,
        };

        Ok(json!({ "result": result }))
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

    async fn execute(&self, args: Value) -> Result<Value> {
        let op = args["operation"].as_str().unwrap_or("add");
        let a = args["a"].as_f64().unwrap_or(0.0);
        let b = args["b"].as_f64().unwrap_or(0.0);

        let result = match op {
            "add" => a + b,
            "subtract" => a - b,
            "multiply" => a * b,
            "divide" => a / b,
            _ => 0.0,
        };

        Ok(json!({ "result": result }))
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
    async fn test_unknown_operation_returns_zero() {
        let tool = Calculator;
        let out = tool
            .execute(json!({"operation": "modulo", "a": 10.0, "b": 3.0}))
            .await
            .unwrap();
        assert_eq!(out["result"], json!(0.0));
    }

    #[tokio::test]
    async fn test_missing_operation_defaults_to_add() {
        let tool = Calculator;
        let out = tool.execute(json!({"a": 2.0, "b": 3.0})).await.unwrap();
        assert_eq!(out["result"], json!(5.0));
    }

    #[tokio::test]
    async fn test_missing_operands_default_to_zero() {
        let tool = Calculator;
        let out = tool.execute(json!({"operation": "add"})).await.unwrap();
        assert_eq!(out["result"], json!(0.0));
    }

    #[tokio::test]
    async fn test_divide_by_zero_serializes_as_null() {
        let tool = Calculator;
        let out = tool
            .execute(json!({"operation": "divide", "a": 1.0, "b": 0.0}))
            .await
            .unwrap();
        // serde_json cannot represent infinity; division by zero yields null.
        assert!(out["result"].is_null());
    }

    // Verify Calculator is resolved through Tools on Context.

    #[test]
    fn test_calculator_service_via_context() {
        use crate::registry::ToolRegistry;
        use crate::Tools;
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
        assert!(resolved
            .list(&isolated)
            .iter()
            .any(|d| d.name == "calculator"));
    }

    #[test]
    fn test_calculator_via_tools_list_resolve() {
        use crate::registry::ToolRegistry;
        use crate::Tools;
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
