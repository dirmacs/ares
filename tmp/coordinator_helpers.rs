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
            !completed.contains(&phase.id) && !failed_phase_ids.contains(&phase.id)
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
    ResearchResult { findings, sources, failed_phase_ids, partial }
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

