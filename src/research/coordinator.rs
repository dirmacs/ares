use crate::{
    llm::{client::TokenUsage, LLMClient},
    types::{Result, Source},
};
use tokio::task::JoinSet;

/// Token usage accumulated across the LLM calls made by a research run.
#[derive(Debug, Clone, Default)]
pub struct ResearchUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl ResearchUsage {
    fn add(&mut self, usage: Option<&TokenUsage>) {
        if let Some(usage) = usage {
            self.input_tokens += usage.prompt_tokens;
            self.output_tokens += usage.completion_tokens;
        }
    }
}

/// Coordinates multi-step research tasks across multiple queries.
///
/// Decomposes research questions, executes parallel searches,
/// and synthesizes findings into a coherent report.
pub struct ResearchCoordinator {
    llm: Box<dyn LLMClient>,
    depth: u8,
    max_iterations: u8,
}

impl ResearchCoordinator {
    /// Creates a new ResearchCoordinator.
    pub fn new(llm: Box<dyn LLMClient>, depth: u8, max_iterations: u8) -> Self {
        Self {
            llm,
            depth,
            max_iterations,
        }
    }

    /// Execute deep research on a query
    pub async fn research(&self, query: &str) -> Result<(String, Vec<Source>)> {
        let (synthesis, sources, _) = self.research_with_usage(query).await?;
        Ok((synthesis, sources))
    }

    /// Execute deep research and return provider-reported token usage.
    pub async fn research_with_usage(
        &self,
        query: &str,
    ) -> Result<(String, Vec<Source>, ResearchUsage)> {
        let mut all_findings = Vec::new();
        let mut usage = ResearchUsage::default();

        // Generate initial research questions
        let (questions, question_usage) = self.generate_research_questions(query).await?;
        usage.add(question_usage.as_ref());

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
                let (follow_ups, followup_usage) = self
                    .generate_followup_questions(query, &all_findings)
                    .await?;
                usage.add(followup_usage.as_ref());

                if follow_ups.is_empty() {
                    break;
                }
            }
        }

        // Synthesize findings
        let (synthesis, synthesis_usage) = self.synthesize_findings(query, &all_findings).await?;
        usage.add(synthesis_usage.as_ref());

        // Extract sources
        let all_sources = self.extract_sources(&all_findings);

        Ok((synthesis, all_sources, usage))
    }

    async fn generate_research_questions(
        &self,
        query: &str,
    ) -> Result<(Vec<String>, Option<TokenUsage>)> {
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

        let response = self
            .llm
            .generate_with_history(&[("user".to_string(), prompt)])
            .await?;

        let questions = response
            .content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                // Remove numbering
                line.trim()
                    .trim_start_matches(|c: char| c.is_numeric() || c == '.' || c == ')')
                    .trim()
                    .to_string()
            })
            .collect();

        Ok((questions, response.usage))
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
    ) -> Result<(Vec<String>, Option<TokenUsage>)> {
        if findings.is_empty() {
            return Ok((vec![], None));
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

        let response = self
            .llm
            .generate_with_history(&[("user".to_string(), prompt)])
            .await?;

        let questions = response
            .content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(3)
            .map(|s| s.to_string())
            .collect();

        Ok((questions, response.usage))
    }

    async fn synthesize_findings(
        &self,
        query: &str,
        findings: &[String],
    ) -> Result<(String, Option<TokenUsage>)> {
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

        let response = self
            .llm
            .generate_with_history(&[("user".to_string(), prompt)])
            .await?;
        Ok((response.content, response.usage))
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
