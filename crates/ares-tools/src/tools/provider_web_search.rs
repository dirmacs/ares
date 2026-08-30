use crate::registry::Tool;
use ares_types::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

/// Marker tool that opts the LLM adapter into provider-native web search.
///
/// This is not daedra. Daedra remains the `web_search` tool (search-tools).
/// AresLlm strips this name from function tools and attaches genai
/// `ToolName::WebSearch`.
pub struct ProviderWebSearch;

#[async_trait]
impl Tool for ProviderWebSearch {
    fn name(&self) -> &str {
        "provider_web_search"
    }

    fn description(&self) -> &str {
        "Enable the LLM provider's built-in web search (OpenAI Responses, Anthropic, Gemini, Ollama, Bedrock). This is not daedra. Daedra remains the `web_search` tool. Provider search is executed by the LLM adapter, not this tool."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: Value) -> Result<Value> {
        Ok(json!({
            "status": "attached",
            "note": "provider web search is executed by the LLM adapter, not this tool"
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn name_is_provider_web_search() {
        assert_eq!(ProviderWebSearch.name(), "provider_web_search");
    }

    #[test]
    fn description_names_providers_and_daedra_split() {
        let description = ProviderWebSearch.description();
        assert!(description.contains("OpenAI Responses"));
        assert!(description.contains("Anthropic"));
        assert!(description.contains("Gemini"));
        assert!(description.contains("Ollama"));
        assert!(description.contains("Bedrock"));
        assert!(description.contains("not daedra"));
        assert!(description.contains("`web_search`"));
    }

    #[test]
    fn parameters_schema_is_empty_object() {
        let schema = ProviderWebSearch.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"], json!({}));
    }

    #[tokio::test]
    async fn execute_returns_attached_without_error() {
        let out = ProviderWebSearch.execute(json!({})).await.unwrap();
        assert_eq!(out["status"], "attached");
        assert_eq!(
            out["note"],
            "provider web search is executed by the LLM adapter, not this tool"
        );
    }

    #[tokio::test]
    async fn mistaken_function_call_with_args_does_not_fail() {
        let out = ProviderWebSearch
            .execute(json!({"query": "should be ignored"}))
            .await
            .unwrap();
        assert_eq!(out["status"], "attached");
    }
}
