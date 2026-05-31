use ares_llm::{client::TokenUsage, LLMClient};
use ares_types::types::{Result, Source};
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

#[cfg(test)]
mod tests {
    use super::*;
    use ares_llm::{client::TokenUsage, LLMClient, LLMResponse};
    use ares_types::types::{AppError, ToolDefinition};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct ResearchMockLlm {
        calls: Arc<AtomicUsize>,
        fail: bool,
    }

    impl ResearchMockLlm {
        fn new() -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                fail: true,
            }
        }
    }

    #[async_trait]
    impl LLMClient for ResearchMockLlm {
        fn model_name(&self) -> &str {
            "research-mock"
        }

        async fn generate(&self, _: &str) -> Result<String> {
            Ok(String::new())
        }

        async fn generate_with_system(&self, _: &str, _: &str) -> Result<String> {
            Ok(String::new())
        }

        async fn generate_with_history(
            &self,
            messages: &[(String, String)],
        ) -> Result<LLMResponse> {
            if self.fail {
                return Err(AppError::Internal("mock llm failure".into()));
            }

            let _n = self.calls.fetch_add(1, Ordering::SeqCst);
            let prompt = messages
                .last()
                .map(|(_, content)| content.as_str())
                .unwrap_or_default();

            let (content, usage) = if prompt.contains("follow-up research questions") {
                (
                    "What is the regulatory timeline?\nWhat are adoption barriers?".to_string(),
                    TokenUsage::new(30, 12),
                )
            } else if prompt.contains("Synthesize these findings") {
                (
                    "Comprehensive synthesized answer.".to_string(),
                    TokenUsage::new(50, 25),
                )
            } else {
                (
                    "1. What is the core technology?\n2. Who are the main vendors?".to_string(),
                    TokenUsage::new(20, 8),
                )
            };

            Ok(LLMResponse {
                content,
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
                usage: Some(usage),
            })
        }

        async fn generate_with_tools(
            &self,
            _: &str,
            _: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: String::new(),
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
                usage: None,
            })
        }

        async fn generate_with_tools_and_history(
            &self,
            _: &[ares_llm::coordinator::ConversationMessage],
            _: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: String::new(),
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
                usage: None,
            })
        }

        async fn stream(
            &self,
            _: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }

        async fn stream_with_system(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }

        async fn stream_with_history(
            &self,
            _: &[(String, String)],
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn test_research_coordinator_end_to_end() {
        let coordinator = ResearchCoordinator::new(Box::new(ResearchMockLlm::new()), 2, 2);
        let (report, sources) = coordinator
            .research("quantum networking trends")
            .await
            .expect("research should succeed");

        assert!(report.contains("Comprehensive synthesized answer"));
        assert_eq!(sources.len(), 4);
        assert_eq!(sources[0].title, "Research Finding 1");
        assert_eq!(sources[0].relevance_score, 0.8);
        assert!(sources[0].url.is_none());
    }

    #[tokio::test]
    async fn test_research_with_usage_accumulates_tokens() {
        let coordinator = ResearchCoordinator::new(Box::new(ResearchMockLlm::new()), 2, 2);
        let (_, _, usage) = coordinator
            .research_with_usage("market landscape")
            .await
            .expect("research_with_usage");

        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 45);
    }

    #[tokio::test]
    async fn test_research_propagates_llm_errors() {
        let coordinator = ResearchCoordinator::new(Box::new(ResearchMockLlm::failing()), 1, 1);
        let err = coordinator
            .research("anything")
            .await
            .expect_err("expected llm failure");
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[test]
    fn test_token_usage_serde_roundtrip() {
        let usage = TokenUsage::new(11, 7);
        let json = serde_json::to_string(&usage).expect("serialize TokenUsage");
        let parsed: TokenUsage = serde_json::from_str(&json).expect("deserialize TokenUsage");
        assert_eq!(parsed, usage);
    }

    #[test]
    fn test_research_usage_default_is_zero() {
        let usage = ResearchUsage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }
}

