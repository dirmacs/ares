//! Configurable Agent implementation
//!
//! This module provides a generic agent that can be configured via TOML.
//! It replaces the hardcoded agent implementations with a flexible,
//! configuration-driven approach.

use crate::agents::Agent;
use crate::llm::LLMClient;
use crate::tools::registry::ToolRegistry;
use crate::types::{AgentContext, AgentType, Result, ToolDefinition};
use crate::utils::toml_config::AgentConfig;
use async_trait::async_trait;
use std::sync::Arc;

/// A configurable agent that derives its behavior from TOML configuration
pub struct ConfigurableAgent {
    /// The agent's name/type identifier
    name: String,
    /// The agent type enum value
    agent_type: AgentType,
    /// The LLM client to use for generation
    llm: Box<dyn LLMClient>,
    /// The system prompt from configuration
    system_prompt: String,
    /// Tools available to this agent
    tool_registry: Option<Arc<ToolRegistry>>,
    /// List of tool names this agent is allowed to use
    allowed_tools: Vec<String>,
    /// Maximum tool calling iterations
    max_tool_iterations: usize,
    /// Whether to execute tools in parallel
    parallel_tools: bool,
}

impl ConfigurableAgent {
    /// Create a new configurable agent from TOML config
    ///
    /// # Arguments
    ///
    /// * `name` - The agent name (used to determine AgentType)
    /// * `config` - The agent configuration from ares.toml
    /// * `llm` - The LLM client (already created from the model config)
    /// * `tool_registry` - Optional tool registry for tool calling
    pub fn new(
        name: &str,
        config: &AgentConfig,
        llm: Box<dyn LLMClient>,
        tool_registry: Option<Arc<ToolRegistry>>,
    ) -> Self {
        let agent_type = Self::name_to_type(name);
        let system_prompt = config
            .system_prompt
            .clone()
            .unwrap_or_else(|| Self::default_system_prompt(name));

        Self {
            name: name.to_string(),
            agent_type,
            llm,
            system_prompt,
            tool_registry,
            allowed_tools: config.tools.clone(),
            max_tool_iterations: config.max_tool_iterations,
            parallel_tools: config.parallel_tools,
        }
    }

    /// Create a new configurable agent with explicit parameters
    #[allow(clippy::too_many_arguments)]
    pub fn with_params(
        name: &str,
        agent_type: AgentType,
        llm: Box<dyn LLMClient>,
        system_prompt: String,
        tool_registry: Option<Arc<ToolRegistry>>,
        allowed_tools: Vec<String>,
        max_tool_iterations: usize,
        parallel_tools: bool,
    ) -> Self {
        Self {
            name: name.to_string(),
            agent_type,
            llm,
            system_prompt,
            tool_registry,
            allowed_tools,
            max_tool_iterations,
            parallel_tools,
        }
    }

    /// Convert agent name to AgentType
    fn name_to_type(name: &str) -> AgentType {
        AgentType::from_string(name)
    }

    /// Get default system prompt for an agent type
    fn default_system_prompt(name: &str) -> String {
        match name.to_lowercase().as_str() {
            "router" => r#"You are a routing agent that classifies user queries.
Available agents: product, invoice, sales, finance, hr, orchestrator.
Respond with ONLY the agent name (one word, lowercase)."#
                .to_string(),

            "orchestrator" => r#"You are an orchestrator agent for complex queries.
Break down requests, delegate to specialists, and synthesize results."#
                .to_string(),

            "product" => r#"You are a Product Agent for product-related queries.
Handle catalog, specifications, inventory, and pricing questions."#
                .to_string(),

            "invoice" => r#"You are an Invoice Agent for billing queries.
Handle invoices, payments, and billing history."#
                .to_string(),

            "sales" => r#"You are a Sales Agent for sales analytics.
Handle performance metrics, revenue, and customer data."#
                .to_string(),

            "finance" => r#"You are a Finance Agent for financial analysis.
Handle statements, budgets, and expense management."#
                .to_string(),

            "hr" => r#"You are an HR Agent for human resources.
Handle employee info, policies, and benefits."#
                .to_string(),

            _ => format!("You are a {} agent.", name),
        }
    }

    /// Get the agent name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the max tool iterations setting
    pub fn max_tool_iterations(&self) -> usize {
        self.max_tool_iterations
    }

    /// Get the parallel tools setting
    pub fn parallel_tools(&self) -> bool {
        self.parallel_tools
    }

    /// Check if this agent has tools configured
    pub fn has_tools(&self) -> bool {
        !self.allowed_tools.is_empty() && self.tool_registry.is_some()
    }

    /// Get the tool registry (if any)
    pub fn tool_registry(&self) -> Option<&Arc<ToolRegistry>> {
        self.tool_registry.as_ref()
    }

    /// Get the list of allowed tool names for this agent
    pub fn allowed_tools(&self) -> &[String] {
        &self.allowed_tools
    }

    /// Get tool definitions for only this agent's allowed tools
    ///
    /// This filters the tool registry to only return tools that:
    /// 1. Are in this agent's allowed tools list
    /// 2. Are enabled in the tool registry
    pub fn get_filtered_tool_definitions(&self) -> Vec<ToolDefinition> {
        match &self.tool_registry {
            Some(registry) => {
                let allowed: Vec<&str> = self.allowed_tools.iter().map(|s| s.as_str()).collect();
                registry.get_tool_definitions_for(&allowed)
            }
            None => Vec::new(),
        }
    }

    /// Check if a specific tool is allowed for this agent
    pub fn can_use_tool(&self, tool_name: &str) -> bool {
        self.allowed_tools.contains(&tool_name.to_string())
            && self
                .tool_registry
                .as_ref()
                .map(|r| r.is_enabled(tool_name))
                .unwrap_or(false)
    }
}

#[async_trait]
impl Agent for ConfigurableAgent {
    async fn execute(&self, input: &str, context: &AgentContext) -> Result<String> {
        // Build context with conversation history if available
        let mut messages = vec![("system".to_string(), self.system_prompt.clone())];

        // Add user memory if available
        if let Some(memory) = &context.user_memory {
            let memory_context = format!(
                "User preferences: {}",
                memory
                    .preferences
                    .iter()
                    .map(|p| format!("{}: {}", p.key, p.value))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            messages.push(("system".to_string(), memory_context));
        }

        // Add recent conversation history (last 5 messages)
        for msg in context.conversation_history.iter().rev().take(5).rev() {
            let role = match msg.role {
                crate::types::MessageRole::User => "user",
                crate::types::MessageRole::Assistant => "assistant",
                _ => "system",
            };
            messages.push((role.to_string(), msg.content.clone()));
        }

        messages.push(("user".to_string(), input.to_string()));

        self.llm.generate_with_history(&messages).await
    }

    fn system_prompt(&self) -> String {
        self.system_prompt.clone()
    }

    fn agent_type(&self) -> AgentType {
        self.agent_type.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name_to_type() {
        assert!(matches!(
            ConfigurableAgent::name_to_type("router"),
            AgentType::Router
        ));
        assert!(matches!(
            ConfigurableAgent::name_to_type("PRODUCT"),
            AgentType::Product
        ));
        // Unknown types now return Custom variant
        assert!(matches!(
            ConfigurableAgent::name_to_type("unknown"),
            AgentType::Custom(_)
        ));
        // Verify the custom name is preserved
        if let AgentType::Custom(name) = ConfigurableAgent::name_to_type("my-custom-agent") {
            assert_eq!(name, "my-custom-agent");
        } else {
            panic!("Expected Custom variant");
        }
    }

    #[test]
    fn test_default_system_prompt() {
        let prompt = ConfigurableAgent::default_system_prompt("router");
        assert!(prompt.contains("routing"));

        let prompt = ConfigurableAgent::default_system_prompt("product");
        assert!(prompt.contains("Product"));
    }

    #[test]
    fn test_allowed_tools() {
        use crate::llm::LLMResponse;
        use crate::utils::toml_config::AgentConfig;
        use std::collections::HashMap;

        // Create a mock LLM client (we'll use a simple mock)
        struct MockLLM;

        #[async_trait]
        impl LLMClient for MockLLM {
            async fn generate(&self, _: &str) -> Result<String> {
                Ok("mock".to_string())
            }
            async fn generate_with_system(&self, _: &str, _: &str) -> Result<String> {
                Ok("mock".to_string())
            }
            async fn generate_with_history(&self, _: &[(String, String)]) -> Result<String> {
                Ok("mock".to_string())
            }
            async fn generate_with_tools(
                &self,
                _: &str,
                _: &[ToolDefinition],
            ) -> Result<LLMResponse> {
                Ok(LLMResponse {
                    content: "mock".to_string(),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                })
            }
            async fn stream(
                &self,
                _: &str,
            ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>
            {
                Ok(Box::new(futures::stream::empty()))
            }
            async fn stream_with_system(
                &self,
                _: &str,
                _: &str,
            ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>
            {
                Ok(Box::new(futures::stream::empty()))
            }
            async fn stream_with_history(
                &self,
                _: &[(String, String)],
            ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>
            {
                Ok(Box::new(futures::stream::empty()))
            }
            fn model_name(&self) -> &str {
                "mock"
            }
        }

        let config = AgentConfig {
            model: "default".to_string(),
            system_prompt: None,
            tools: vec!["calculator".to_string(), "web_search".to_string()],
            max_tool_iterations: 5,
            parallel_tools: false,
            extra: HashMap::new(),
        };

        let agent = ConfigurableAgent::new(
            "orchestrator",
            &config,
            Box::new(MockLLM),
            None, // No registry for this test
        );

        assert_eq!(agent.allowed_tools().len(), 2);
        assert!(agent.allowed_tools().contains(&"calculator".to_string()));
        assert!(agent.allowed_tools().contains(&"web_search".to_string()));
    }

    #[test]
    fn test_has_tools_requires_both_config_and_registry() {
        use crate::llm::LLMResponse;
        use crate::utils::toml_config::AgentConfig;
        use std::collections::HashMap;

        struct MockLLM;

        #[async_trait]
        impl LLMClient for MockLLM {
            async fn generate(&self, _: &str) -> Result<String> {
                Ok("mock".to_string())
            }
            async fn generate_with_system(&self, _: &str, _: &str) -> Result<String> {
                Ok("mock".to_string())
            }
            async fn generate_with_history(&self, _: &[(String, String)]) -> Result<String> {
                Ok("mock".to_string())
            }
            async fn generate_with_tools(
                &self,
                _: &str,
                _: &[ToolDefinition],
            ) -> Result<LLMResponse> {
                Ok(LLMResponse {
                    content: "mock".to_string(),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                })
            }
            async fn stream(
                &self,
                _: &str,
            ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>
            {
                Ok(Box::new(futures::stream::empty()))
            }
            async fn stream_with_system(
                &self,
                _: &str,
                _: &str,
            ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>
            {
                Ok(Box::new(futures::stream::empty()))
            }
            async fn stream_with_history(
                &self,
                _: &[(String, String)],
            ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>
            {
                Ok(Box::new(futures::stream::empty()))
            }
            fn model_name(&self) -> &str {
                "mock"
            }
        }

        // Agent with tools config but no registry
        let config = AgentConfig {
            model: "default".to_string(),
            system_prompt: None,
            tools: vec!["calculator".to_string()],
            max_tool_iterations: 5,
            parallel_tools: false,
            extra: HashMap::new(),
        };

        let agent = ConfigurableAgent::new("orchestrator", &config, Box::new(MockLLM), None);
        assert!(!agent.has_tools()); // No registry

        // Agent with empty tools
        let config_empty = AgentConfig {
            model: "default".to_string(),
            system_prompt: None,
            tools: vec![],
            max_tool_iterations: 5,
            parallel_tools: false,
            extra: HashMap::new(),
        };

        let agent_empty = ConfigurableAgent::new(
            "product",
            &config_empty,
            Box::new(MockLLM),
            Some(Arc::new(ToolRegistry::new())),
        );
        assert!(!agent_empty.has_tools()); // Empty tools list
    }
}
