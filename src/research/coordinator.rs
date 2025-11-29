use crate::{
    llm::LLMClient,
    types::{AppError, Result, Source},
};
use tokio::task::JoinSet;

pub struct ResearchCoordinator {
    llm: Box<dyn LLMClient>,
    depth: u8,
    max_iterations: u8,
}

impl ResearchCoordinator {
    pub fn new(llm: Box<dyn LLMCLient>, depth: u8, max_iterations: u8) -> Self {
        Self {
            llm,
            depth,
            max_iterations,
        }
    }

    /// Execute deep research on a query
    pub async fn research(&self, query: &str) -> Result<(String, Vec<Source>)> {
        let mut all_findings = Vec::new();
        let mut all_sources = Vec::new();

        // Generate initial research questions
        // let questions = self.generate_research
    }

    async fn generate_research_questions(&self, query: &str) -> Result<Vec<String>> {
        // let prompt = format!(

        // )
    }
}
