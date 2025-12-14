//! Configurable Agent implementation
//!
//! This module provides a generic agent that can be configured via TOML.
//! It replaces the hardcoded agent implementations with a flexible,
//! configuration-driven approach.

use crate::agents::Agent;
use crate::llm::LLMClient;
use crate::tools::registry::ToolRegistry;
use crate::types::{AgentContext, AgentType, Result};
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
            max_tool_iterations: config.max_tool_iterations,
            parallel_tools: config.parallel_tools,
        }
    }

    /// Create a new configurable agent with explicit parameters
    pub fn with_params(
        name: &str,
        agent_type: AgentType,
        llm: Box<dyn LLMClient>,
        system_prompt: String,
        tool_registry: Option<Arc<ToolRegistry>>,
        max_tool_iterations: usize,
        parallel_tools: bool,
    ) -> Self {
        Self {
            name: name.to_string(),
            agent_type,
            llm,
            system_prompt,
            tool_registry,
            max_tool_iterations,
            parallel_tools,
        }
    }

    /// Convert agent name to AgentType
    fn name_to_type(name: &str) -> AgentType {
        match name.to_lowercase().as_str() {
            "router" => AgentType::Router,
            "orchestrator" => AgentType::Orchestrator,
            "product" => AgentType::Product,
            "invoice" => AgentType::Invoice,
            "sales" => AgentType::Sales,
            "finance" => AgentType::Finance,
            "hr" => AgentType::HR,
            _ => AgentType::Orchestrator, // Default to orchestrator for unknown types
        }
    }

    /// Get default system prompt for an agent type
    fn default_system_prompt(name: &str) -> String {
        match name.to_lowercase().as_str() {
            "router" => r#"You are a routing agent that classifies user queries.
Available agents: product, invoice, sales, finance, hr, orchestrator.
Respond with ONLY the agent name (one word, lowercase)."#.to_string(),
            
            "orchestrator" => r#"You are an orchestrator agent for complex queries.
Break down requests, delegate to specialists, and synthesize results."#.to_string(),
            
            "product" => r#"You are a Product Agent for product-related queries.
Handle catalog, specifications, inventory, and pricing questions."#.to_string(),
            
            "invoice" => r#"You are an Invoice Agent for billing queries.
Handle invoices, payments, and billing history."#.to_string(),
            
            "sales" => r#"You are a Sales Agent for sales analytics.
Handle performance metrics, revenue, and customer data."#.to_string(),
            
            "finance" => r#"You are a Finance Agent for financial analysis.
Handle statements, budgets, and expense management."#.to_string(),
            
            "hr" => r#"You are an HR Agent for human resources.
Handle employee info, policies, and benefits."#.to_string(),
            
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
        self.tool_registry.is_some()
    }

    /// Get the tool registry (if any)
    pub fn tool_registry(&self) -> Option<&Arc<ToolRegistry>> {
        self.tool_registry.as_ref()
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
        self.agent_type
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
        assert!(matches!(
            ConfigurableAgent::name_to_type("unknown"),
            AgentType::Orchestrator
        ));
    }

    #[test]
    fn test_default_system_prompt() {
        let prompt = ConfigurableAgent::default_system_prompt("router");
        assert!(prompt.contains("routing"));

        let prompt = ConfigurableAgent::default_system_prompt("product");
        assert!(prompt.contains("Product"));
    }
}
