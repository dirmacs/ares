use std::collections::HashSet;
use std::fmt;

use ares_llm::{client::TokenUsage, LLMClient};
use ares_types::types::{Result, Source};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

/// How sub-queries within a research plan are scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseExecutionMode {
    Sequential,
    Parallel,
}

/// A single research step derived from a parent query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchPhase {
    pub id: String,
    pub query: String,
    pub mode: PhaseExecutionMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

/// Ordered research work units for a root question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchPlan {
    pub root_query: String,
    pub phases: Vec<ResearchPhase>,
}

/// One phase's output before aggregation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchFinding {
    pub phase_id: String,
    pub content: String,
    pub relevance_score: f32,
}

/// Aggregated outcome of executing a [`ResearchPlan`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchResult {
    pub findings: Vec<ResearchFinding>,
    pub sources: Vec<Source>,
    pub failed_phase_ids: Vec<String>,
    pub partial: bool,
}

/// Inputs for [`rank_sources`] (recency, authority, and query match).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceCandidate {
    pub source: Source,
    pub published_epoch_secs: Option<u64>,
    pub authority_score: f32,
    pub content_match_score: f32,
}

impl fmt::Display for ResearchPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "ResearchPlan: {}", self.root_query)?;
        for phase in &self.phases {
            let deps = if phase.depends_on.is_empty() {
                String::new()
            } else {
                format!(" (after {})", phase.depends_on.join(", "))
            };
            writeln!(f, "  [{} / {:?}] {}{}", phase.id, phase.mode, phase.query, deps)?;
        }
        Ok(())
    }
}

impl fmt::Display for ResearchPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ResearchPhase {} [{:?}]: {}", self.id, self.mode, self.query)
    }
}

impl fmt::Display for ResearchResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "ResearchResult: {} finding(s), {} source(s), partial={}", self.findings.len(), self.sources.len(), self.partial)?;
        if !self.failed_phase_ids.is_empty() {
            writeln!(f, "  failed phases: {}", self.failed_phase_ids.join(", "))?;
        }
        for finding in &self.findings {
            writeln!(f, "  - {} (relevance {:.2}): {}", finding.phase_id, finding.relevance_score, finding.content)?;
        }
        Ok(())
    }
}

pub fn plan_research_phases(root_query: &str, sub_queries: &[String], parallel: bool) -> ResearchPlan {
    let mode = if parallel { PhaseExecutionMode::Parallel } else { PhaseExecutionMode::Sequential };
    let phases = sub_queries.iter().enumerate().map(|(index, query)| {
        let id = format!("phase-{}", index + 1);
        let depends_on = if parallel || index == 0 { Vec::new() } else { vec![format!("phase-{index}")] };
        ResearchPhase { id, query: query.clone(), mode, depends_on }
    }).collect();
    ResearchPlan { root_query: root_query.to_string(), phases }
}

pub fn execute_phase<F>(phase: &ResearchPhase, executor: &mut F) -> std::result::Result<ResearchFinding, String>
where
    F: FnMut(&ResearchPhase) -> std::result::Result<String, String>,
{
    let content = executor(phase)?;
    Ok(ResearchFinding {
        phase_id: phase.id.clone(),
        relevance_score: score_content_relevance(&phase.query, &content),
        content,
    })
}

pub fn run_research_plan<F>(plan: &ResearchPlan, executor: &mut F) -> ResearchResult
where
    F: FnMut(&ResearchPhase) -> std::result::Result<String, String>,
{
    let mut completed = HashSet::new();
    let mut raw_findings = Vec::new();
    let mut failed_phase_ids = Vec::new();
    loop {
        let ready: Vec<&ResearchPhase> = plan.phases.iter().filter(|phase| {
            !completed.contains(&phase.id)
                    && !failed_phase_ids.contains(&phase.id)
                    && phase.depends_on.iter().all(|dep| completed.contains(dep))
        }).collect();
        if ready.is_empty() { break; }
        let parallel = ready[0].mode == PhaseExecutionMode::Parallel;
        let batch: Vec<&ResearchPhase> = if parallel { ready } else { vec![ready[0]] };
        for phase in batch {
            match execute_phase(phase, executor) {
                Ok(finding) => { completed.insert(phase.id.clone()); raw_findings.push(finding); }
                Err(_) => { failed_phase_ids.push(phase.id.clone()); }
            }
        }
    }
    let findings = aggregate_findings(&raw_findings);
    let candidates = findings_to_source_candidates(&findings, &plan.root_query);
    let sources = rank_sources(&candidates, &plan.root_query, current_epoch_secs());
    let partial = !failed_phase_ids.is_empty() && !findings.is_empty();

    ResearchResult {
        findings,
        sources,
        failed_phase_ids,
        partial,
    }
}

pub fn aggregate_findings(findings: &[ResearchFinding]) -> Vec<ResearchFinding> {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for finding in findings {
        let key = normalize_content(&finding.content);
        if key.is_empty() || !seen.insert(key) { continue; }
        unique.push(finding.clone());
    }
    unique.sort_by(|left, right| right.relevance_score.partial_cmp(&left.relevance_score).unwrap_or(std::cmp::Ordering::Equal));
    unique
}

pub fn rank_sources(candidates: &[SourceCandidate], query: &str, now_epoch_secs: u64) -> Vec<Source> {
    let mut scored: Vec<(f32, &SourceCandidate)> = candidates.iter().map(|candidate| {
        let recency = recency_score(candidate.published_epoch_secs, now_epoch_secs);
        let content = candidate.content_match_score.max(score_content_relevance(query, &candidate.source.title));
        let composite = 0.45 * candidate.authority_score + 0.30 * recency + 0.25 * content;
        (composite, candidate)
    }).collect();
    scored.sort_by(|(left, _), (right, _)| right.partial_cmp(left).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(score, candidate)| {
        let mut source = candidate.source.clone();
        source.relevance_score = score.clamp(0.0, 1.0);
        source
    }).collect()
}

pub fn score_content_relevance(query: &str, content: &str) -> f32 {
    let query_tokens = tokenize(query);
    if query_tokens.is_empty() { return 0.0; }
    let content_tokens = tokenize(content);
    let overlap = query_tokens.iter().filter(|token| content_tokens.contains(*token)).count();
    (overlap as f32 / query_tokens.len() as f32).clamp(0.0, 1.0)
}

fn recency_score(published_epoch_secs: Option<u64>, now_epoch_secs: u64) -> f32 {
    published_epoch_secs.map(|published| {
        let age_days = now_epoch_secs.saturating_sub(published) / 86_400;
        (1.0 - (age_days as f32 / 365.0)).clamp(0.0, 1.0)
    }).unwrap_or(0.5)
}

fn score_authority(url: Option<&str>) -> f32 {
    let Some(url) = url else { return 0.3; };
    let lower = url.to_ascii_lowercase();
    if lower.contains(".gov") || lower.contains(".edu") { 1.0 }
    else if lower.contains("arxiv.org") || lower.contains("doi.org") || lower.contains(".org") { 0.85 }
    else { 0.55 }
}

fn normalize_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
}

fn tokenize(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|token| token.len() >= 3).map(|token| token.to_ascii_lowercase()).collect()
}

fn extract_url(text: &str) -> Option<String> {
    text.split_whitespace().find(|word| word.starts_with("http://") || word.starts_with("https://")).map(str::to_string)
}

fn extract_published_epoch(text: &str) -> Option<u64> {
    text.split_whitespace().find_map(|word| word.strip_prefix("published:").and_then(|value| value.parse().ok()))
}

fn findings_to_source_candidates(findings: &[ResearchFinding], root_query: &str) -> Vec<SourceCandidate> {
    findings.iter().enumerate().map(|(index, finding)| {
        let url = extract_url(&finding.content);
        let authority_score = score_authority(url.as_deref());
        let content_match_score = score_content_relevance(root_query, &finding.content).max(finding.relevance_score);
        SourceCandidate {
            source: Source { title: format!("Research Finding {}", index + 1), url, relevance_score: content_match_score },
            published_epoch_secs: extract_published_epoch(&finding.content),
            authority_score,
            content_match_score,
        }
    }).collect()
}

fn current_epoch_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_secs()).unwrap_or(0)
}

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
        let all_sources = self.extract_sources(query, &all_findings);

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

    fn extract_sources(&self, query: &str, findings: &[String]) -> Vec<Source> {
        let research_findings: Vec<ResearchFinding> = findings
            .iter()
            .enumerate()
            .map(|(index, finding)| ResearchFinding {
                phase_id: format!("finding-{}", index + 1),
                content: finding.clone(),
                relevance_score: score_content_relevance(query, finding),
            })
            .collect();

        let aggregated = aggregate_findings(&research_findings);
        let candidates = findings_to_source_candidates(&aggregated, query);
        rank_sources(&candidates, query, current_epoch_secs())
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


    fn sample_sub_queries() -> Vec<String> {
        vec![
            "What is the core technology?".to_string(),
            "Who are the main vendors?".to_string(),
            "What is the market size?".to_string(),
        ]
    }

    #[test]
    fn research_plan_serde_roundtrip() {
        let plan = plan_research_phases("quantum networking", &sample_sub_queries(), true);
        let json = serde_json::to_string(&plan).expect("serialize plan");
        let parsed: ResearchPlan = serde_json::from_str(&json).expect("deserialize plan");
        assert_eq!(parsed, plan);
    }

    #[test]
    fn research_phase_serde_roundtrip() {
        let phase = ResearchPhase {
            id: "phase-1".into(),
            query: "vendors".into(),
            mode: PhaseExecutionMode::Sequential,
            depends_on: vec![],
        };
        let json = serde_json::to_string(&phase).expect("serialize phase");
        let parsed: ResearchPhase = serde_json::from_str(&json).expect("deserialize phase");
        assert_eq!(parsed, phase);
    }

    #[test]
    fn research_result_serde_roundtrip() {
        let result = ResearchResult {
            findings: vec![ResearchFinding {
                phase_id: "phase-1".into(),
                content: "finding".into(),
                relevance_score: 0.9,
            }],
            sources: vec![Source {
                title: "Paper".into(),
                url: Some("https://example.edu/paper".into()),
                relevance_score: 0.9,
            }],
            failed_phase_ids: vec!["phase-2".into()],
            partial: true,
        };
        let json = serde_json::to_string(&result).expect("serialize result");
        let parsed: ResearchResult = serde_json::from_str(&json).expect("deserialize result");
        assert_eq!(parsed.findings, result.findings);
        assert_eq!(parsed.failed_phase_ids, result.failed_phase_ids);
        assert_eq!(parsed.partial, result.partial);
        assert_eq!(parsed.sources.len(), result.sources.len());
    }

    #[test]
    fn phase_execution_mode_serde_roundtrip() {
        for mode in [PhaseExecutionMode::Sequential, PhaseExecutionMode::Parallel] {
            let json = serde_json::to_string(&mode).expect("serialize mode");
            let parsed: PhaseExecutionMode = serde_json::from_str(&json).expect("deserialize mode");
            assert_eq!(parsed, mode);
        }
    }

    #[test]
    fn plan_research_phases_parallel_has_no_dependencies() {
        let plan = plan_research_phases("root", &sample_sub_queries(), true);
        assert!(plan.phases.iter().all(|phase| phase.depends_on.is_empty()));
        assert!(plan.phases.iter().all(|phase| phase.mode == PhaseExecutionMode::Parallel));
    }

    #[test]
    fn plan_research_phases_sequential_builds_dependency_chain() {
        let plan = plan_research_phases("root", &sample_sub_queries(), false);
        assert_eq!(plan.phases[0].depends_on, Vec::<String>::new());
        assert_eq!(plan.phases[1].depends_on, vec!["phase-1".to_string()]);
        assert_eq!(plan.phases[2].depends_on, vec!["phase-2".to_string()]);
        assert!(plan.phases.iter().all(|phase| phase.mode == PhaseExecutionMode::Sequential));
    }

    #[test]
    fn plan_research_phases_preserves_sub_queries() {
        let queries = sample_sub_queries();
        let plan = plan_research_phases("root", &queries, true);
        let planned: Vec<_> = plan.phases.iter().map(|phase| phase.query.clone()).collect();
        assert_eq!(planned, queries);
    }

    #[test]
    fn execute_phase_success_scores_relevance() {
        let phase = ResearchPhase {
            id: "phase-1".into(),
            query: "quantum error correction".into(),
            mode: PhaseExecutionMode::Parallel,
            depends_on: vec![],
        };
        let mut executor = |_: &ResearchPhase| {
            Ok("quantum error correction advances published:1700000000 https://arxiv.org/abs/123".into())
        };
        let finding = execute_phase(&phase, &mut executor).expect("phase should succeed");
        assert_eq!(finding.phase_id, "phase-1");
        assert!(finding.relevance_score > 0.5);
    }

    #[test]
    fn execute_phase_failure_returns_error() {
        let phase = ResearchPhase {
            id: "phase-1".into(),
            query: "vendors".into(),
            mode: PhaseExecutionMode::Parallel,
            depends_on: vec![],
        };
        let mut executor = |_: &ResearchPhase| Err("upstream search unavailable".to_string());
        let err = execute_phase(&phase, &mut executor).expect_err("phase should fail");
        assert!(err.contains("unavailable"));
    }

    #[test]
    fn aggregate_findings_deduplicates_normalized_content() {
        let findings = vec![
            ResearchFinding { phase_id: "a".into(), content: "Same   content here".into(), relevance_score: 0.4 },
            ResearchFinding { phase_id: "b".into(), content: "same content here".into(), relevance_score: 0.9 },
        ];
        let aggregated = aggregate_findings(&findings);
        assert_eq!(aggregated.len(), 1);
        assert_eq!(aggregated[0].phase_id, "b");
    }

    #[test]
    fn aggregate_findings_sorts_by_relevance_descending() {
        let findings = vec![
            ResearchFinding { phase_id: "low".into(), content: "alpha".into(), relevance_score: 0.2 },
            ResearchFinding { phase_id: "high".into(), content: "beta".into(), relevance_score: 0.95 },
        ];
        let aggregated = aggregate_findings(&findings);
        assert_eq!(aggregated[0].phase_id, "high");
        assert_eq!(aggregated[1].phase_id, "low");
    }

    #[test]
    fn aggregate_findings_empty_input_returns_empty() {
        assert!(aggregate_findings(&[]).is_empty());
    }

    #[test]
    fn score_content_relevance_handles_no_tokens() {
        assert_eq!(score_content_relevance("a", "beta gamma delta"), 0.0);
    }

    #[test]
    fn rank_sources_prefers_higher_authority() {
        let now = 1_700_000_000;
        let candidates = vec![
            SourceCandidate {
                source: Source { title: "Blog".into(), url: Some("https://example.com/post".into()), relevance_score: 0.0 },
                published_epoch_secs: Some(now), authority_score: 0.55, content_match_score: 0.5,
            },
            SourceCandidate {
                source: Source { title: "Government report".into(), url: Some("https://agency.gov/report".into()), relevance_score: 0.0 },
                published_epoch_secs: Some(now), authority_score: 1.0, content_match_score: 0.5,
            },
        ];
        let ranked = rank_sources(&candidates, "report", now);
        assert!(ranked[0].url.as_deref().unwrap().contains(".gov"));
    }

    #[test]
    fn rank_sources_prefers_more_recent_publication() {
        let now = 1_700_000_000;
        let candidates = vec![
            SourceCandidate {
                source: Source { title: "Old".into(), url: Some("https://example.com/old".into()), relevance_score: 0.0 },
                published_epoch_secs: Some(now - 400 * 86_400), authority_score: 0.55, content_match_score: 0.6,
            },
            SourceCandidate {
                source: Source { title: "New".into(), url: Some("https://example.com/new".into()), relevance_score: 0.0 },
                published_epoch_secs: Some(now - 7 * 86_400), authority_score: 0.55, content_match_score: 0.6,
            },
        ];
        let ranked = rank_sources(&candidates, "example", now);
        assert_eq!(ranked[0].title, "New");
    }

    #[test]
    fn rank_sources_prefers_better_content_match() {
        let now = 1_700_000_000;
        let candidates = vec![
            SourceCandidate {
                source: Source { title: "Unrelated".into(), url: None, relevance_score: 0.0 },
                published_epoch_secs: Some(now), authority_score: 0.55, content_match_score: 0.1,
            },
            SourceCandidate {
                source: Source { title: "Quantum networking overview".into(), url: None, relevance_score: 0.0 },
                published_epoch_secs: Some(now), authority_score: 0.55, content_match_score: 0.95,
            },
        ];
        let ranked = rank_sources(&candidates, "quantum networking", now);
        assert!(ranked[0].title.contains("Quantum"));
    }

    #[test]
    fn run_research_plan_parallel_collects_all_phases() {
        let plan = plan_research_phases("root", &sample_sub_queries(), true);
        let mut executor = |phase: &ResearchPhase| Ok(format!("answer for {}", phase.id));
        let result = run_research_plan(&plan, &mut executor);
        assert_eq!(result.findings.len(), 3);
        assert!(!result.partial);
        assert!(result.failed_phase_ids.is_empty());
    }

    #[test]
    fn run_research_plan_sequential_stops_after_failure() {
        let plan = plan_research_phases("root", &["first".into(), "second".into(), "third".into()], false);
        let mut executor = |phase: &ResearchPhase| {
            if phase.id == "phase-2" { Err("phase two failed".into()) } else { Ok(format!("ok {}", phase.id)) }
        };
        let result = run_research_plan(&plan, &mut executor);
        assert_eq!(result.findings.len(), 1);
        assert!(result.partial);
        assert_eq!(result.failed_phase_ids, vec!["phase-2".to_string()]);
    }

    #[test]
    fn run_research_plan_partial_success_flag() {
        let plan = plan_research_phases("root", &["only".into()], true);
        let mut ok = |_: &ResearchPhase| Ok("content".into());
        assert!(!run_research_plan(&plan, &mut ok).partial);
        let mut fail = |_: &ResearchPhase| Err("boom".into());
        let failure = run_research_plan(&plan, &mut fail);
        assert!(!failure.partial);
        assert!(failure.findings.is_empty());
    }

    #[test]
    fn research_plan_display_includes_phases() {
        let plan = plan_research_phases("quantum", &["vendors".into()], false);
        let text = plan.to_string();
        assert!(text.contains("ResearchPlan: quantum"));
        assert!(text.contains("phase-1"));
    }

    #[test]
    fn research_phase_display_formats_id_and_query() {
        let phase = ResearchPhase { id: "phase-9".into(), query: "market size".into(), mode: PhaseExecutionMode::Parallel, depends_on: vec![] };
        let text = phase.to_string();
        assert!(text.contains("phase-9"));
        assert!(text.contains("market size"));
    }

    #[test]
    fn research_result_display_lists_findings() {
        let result = ResearchResult {
            findings: vec![ResearchFinding { phase_id: "phase-1".into(), content: "data".into(), relevance_score: 0.5 }],
            sources: vec![], failed_phase_ids: vec![], partial: false,
        };
        let text = result.to_string();
        assert!(text.contains("1 finding(s)"));
        assert!(text.contains("phase-1"));
    }

    #[test]
    fn research_plan_clone_and_debug() {
        let plan = plan_research_phases("root", &["q".into()], true);
        let cloned = plan.clone();
        assert_eq!(format!("{plan:?}"), format!("{cloned:?}"));
    }

    #[test]
    fn research_usage_debug_clone() {
        let usage = ResearchUsage { input_tokens: 3, output_tokens: 7 };
        let cloned = usage.clone();
        assert_eq!(format!("{usage:?}"), format!("{cloned:?}"));
    }

    #[test]
    fn score_authority_boosts_edu_and_gov_domains() {
        assert_eq!(score_authority(Some("https://university.edu/paper")), 1.0);
        assert!(score_authority(Some("https://blog.example.com")) < 1.0);
    }

    #[test]
    fn findings_to_source_candidates_extracts_url_and_epoch() {
        let findings = vec![ResearchFinding {
            phase_id: "phase-1".into(),
            content: "quantum paper published:1700000000 https://arxiv.org/abs/123".into(),
            relevance_score: 0.8,
        }];
        let candidates = findings_to_source_candidates(&findings, "quantum paper");
        assert_eq!(candidates[0].source.url.as_deref(), Some("https://arxiv.org/abs/123"));
        assert_eq!(candidates[0].published_epoch_secs, Some(1_700_000_000));
        assert!(candidates[0].authority_score >= 0.85);
    }

    #[tokio::test]
    async fn test_research_coordinator_end_to_end() {
        let coordinator = ResearchCoordinator::new(Box::new(ResearchMockLlm::new()), 2, 2);
        let (report, sources) = coordinator
            .research("quantum networking trends")
            .await
            .expect("research should succeed");

        assert!(report.contains("Comprehensive synthesized answer"));
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].title, "Research Finding 1");
        assert!(sources[0].relevance_score > 0.0);
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

    #[tokio::test]
    async fn test_generate_followup_questions_empty_findings() {
        let coordinator = ResearchCoordinator::new(Box::new(ResearchMockLlm::new()), 2, 2);
        let (questions, usage) = coordinator
            .generate_followup_questions("original query", &[])
            .await
            .expect("empty findings should succeed");
        assert!(questions.is_empty());
        assert!(usage.is_none());
    }
}

