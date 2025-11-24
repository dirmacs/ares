use crate::{
    agents::Agent,
    llm::LLMClient,
    types::{AgentContext, AgentType, Result},
};
use async_trait::async_trait;

pub struct ProductAgent {
    llm: Box<dyn LLMClient>,
}

impl ProductAgent {
    pub fn new(llm: Box<dyn LLMClient>) -> Self {
        Self { llm }
    }
}

#[async_trait]
impl Agent for ProductAgent {
    async fn execute(&self, input: &str, context: &AgentContext) -> Result<String> {
        let system_prompt = self.system_prompt();

        // Build context with conversation history
        let mut messages = vec![("system".to_string(), system_prompt)];

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
        r#"You are a Product Agent specialized in handling product-related queries.

Your capabilities:
- Product catalog search and recommendations
- Product specifications and details
- Inventory status and availability
- Product comparisons and alternatives
- Pricing information
- Product category navigation

Always provide accurate, helpful information about products. If you don't have specific product data, suggest how the user can find it or offer to help with related queries."#.to_string()
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Product
    }
}
