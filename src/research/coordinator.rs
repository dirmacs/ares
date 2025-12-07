use crate::{
    llm::LLMClient,
    types::{Result, Source},
};
use tokio::task::JoinSet;

pub struct ResearchCoordinator {
    llm: Box<dyn LLMClient>,
    depth: u8,
    max_iterations: u8,
}

impl ResearchCoordinator {
    pub fn new(llm: Box<dyn LLMClient>, depth: u8, max_iterations: u8) -> Self {
        Self {
            llm,
            depth,
            max_iterations,
        }
    }

    /// Execute deep research on a query
    pub async fn research(&self, query: &str) -> Result<(String, Vec<Source>)> {
        let mut all_findings = Vec::new();

        // Generate initial research questions
        let questions = self.generate_research_questions(query).await?;

        // Execute breadth-first parallel search
        for iteration in 0..self.max_iterations {
            tracing::info!(
                "Research iteration {}/{}",
                iteration + 1,
                self.max_iterations
            );

            let findings = self.parallel_research(&questions).await?;
            all_findings.extend(findings);

            // Check if we have enough information
            if all_findings.len() >= (self.depth as usize * 3) {
                break;
            }

            // Generate follow-up questions based on findings
            if iteration < self.max_iterations - 1 {
                let follow_ups = self
                    .generate_followup_questions(query, &all_findings)
                    .await?;

                if follow_ups.is_empty() {
                    break;
                }
            }
        }

        // Synthesize findings
        let synthesis = self.synthesize_findings(query, &all_findings).await?;

        // Extract sources
        let all_sources = self.extract_sources(&all_findings);

        Ok((synthesis, all_sources))
    }

    async fn generate_research_questions(&self, query: &str) -> Result<Vec<String>> {
        let prompt = format!(
            r#"Generate {} focused research questions to comprehensively answer: {}

Return only the questions, one per line, numbered 1-{}.

Example:

1. [QUESTION 1]
2. [QUESTION 2]
3. [QUESTION 3]
..."#,
            self.depth, query, self.depth
        );

        let response = self.llm.generate(&prompt).await?;

        Ok(response
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                // Remove numbering
                line.trim()
                    .trim_start_matches(|c: char| c.is_numeric() || c == '.' || c == ')')
                    .trim()
                    .to_string()
            })
            .collect())
    }

    async fn parallel_research(&self, questions: &[String]) -> Result<Vec<String>> {
        let mut set = JoinSet::new();

        for question in questions.iter().take(self.depth as usize) {
            let question = question.clone();
            let _llm_clone = self.llm.model_name().to_string(); // Simplified for example

            set.spawn(async move {
                // Simplified research - in production, this would call web search tools
                format!("Research findings for: {}", question)
            });
        }

        let mut results = Vec::new();
        while let Some(res) = set.join_next().await {
            if let Ok(finding) = res {
                results.push(finding);
            }
        }

        Ok(results)
    }

    async fn generate_followup_questions(
        &self,
        _original_query: &str,
        findings: &[String],
    ) -> Result<Vec<String>> {
        if findings.is_empty() {
            return Ok(vec![]);
        }

        let prompt = format!(
            r#"Based on these findings:
    {}

    Generate 2-3 follow-up research questions.

    ONLY output the questions and nothing else, like this:

    <question1>
    <question2>
    <question3>

    "#,
            findings.join("\n")
        );

        let response = self.llm.generate(&prompt).await?;

        Ok(response
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(3)
            .map(|s| s.to_string())
            .collect())
    }

    async fn synthesize_findings(&self, query: &str, findings: &[String]) -> Result<String> {
        let prompt = format!(
            r#"Original query: {}

      Research findings:
      {}

      Synthesize these findings into a comprehensive, well-structured answer. Include:
      1. Direct answer to the question
      2. Key insights
      3. Supporting evidence
      4. Caveats or limitations if any

      Provide a clear, professional response."#,
            query,
            findings.join("\n\n")
        );

        self.llm.generate(&prompt).await
    }

    fn extract_sources(&self, findings: &[String]) -> Vec<Source> {
        // Simplified source extraction
        findings
            .iter()
            .enumerate()
            .map(|(i, _finding)| Source {
                title: format!("Research Finding {}", i + 1),
                url: None,
                relevance_score: 0.8,
            })
            .collect()
    }
}
