use crate::registry::Tool;
use ares_types::Result;
use async_trait::async_trait;
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
        assert_eq!(
            schema["required"],
            json!(["operation", "a", "b"])
        );
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
        let out = tool
            .execute(json!({"a": 2.0, "b": 3.0}))
            .await
            .unwrap();
        assert_eq!(out["result"], json!(5.0));
    }

    #[tokio::test]
    async fn test_missing_operands_default_to_zero() {
        let tool = Calculator;
        let out = tool
            .execute(json!({"operation": "add"}))
            .await
            .unwrap();
        assert_eq!(out["result"], json!(0.0));
    }

    #[tokio::test]
    async fn test_divide_by_zero_serializes_as_null() {
        let tool = Calculator;
        let out = tool
            .execute(json!({"operation": "divide", "a": 1.0, "b": 0.0}))
            .await
            .unwrap();
        // serde_json cannot represent infinity; f64 division yields null in output.
        assert!(out["result"].is_null());
    }
}
