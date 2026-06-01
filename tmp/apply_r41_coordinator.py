#!/usr/bin/env python3
from pathlib import Path

TARGET = Path("/opt/ares/crates/ares-agents/src/research/coordinator.rs")
HELPERS = Path("/opt/ares/tmp/coordinator_helpers.rs")

def main():
    text = TARGET.read_text()
    helpers = HELPERS.read_text()
    marker = "/// Token usage accumulated across the LLM calls made by a research run."
    if "PhaseExecutionMode" not in text:
        if "use std::collections::HashSet" not in text:
            text = text.replace(
                "use ares_llm::{client::TokenUsage, LLMClient};",
                "use std::collections::HashSet;
use std::fmt;

use ares_llm::{client::TokenUsage, LLMClient};",
                1,
            )
            text = text.replace(
                "use ares_types::types::{Result, Source};",
                "use ares_types::types::{Result, Source};
use serde::{Deserialize, Serialize};",
                1,
            )
        text = text.replace(marker, helpers + marker, 1)
    old_extract = """    fn extract_sources(&self, findings: &[String]) -> Vec<Source> {
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
    }"""
    new_extract = """    fn extract_sources(&self, query: &str, findings: &[String]) -> Vec<Source> {
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
    }"""
    if "extract_sources(&self, query:" not in text:
        text = text.replace(old_extract, new_extract, 1)
        text = text.replace(
            "let all_sources = self.extract_sources(&all_findings);",
            "let all_sources = self.extract_sources(query, &all_findings);",
            1,
        )
    tests = Path("/opt/ares/tmp/coordinator_tests.rs")
    if tests.exists() and "fn sample_sub_queries()" not in text:
        anchor = "    #[tokio::test]
    async fn test_research_coordinator_end_to_end()"
        text = text.replace(anchor, tests.read_text() + anchor, 1)
    e2e_old = """        assert_eq!(sources.len(), 4);
        assert_eq!(sources[0].title, "Research Finding 1");
        assert_eq!(sources[0].relevance_score, 0.8);
        assert!(sources[0].url.is_none());"""
    e2e_new = """        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].title, "Research Finding 1");
        assert!(sources[0].relevance_score > 0.0);"""
    text = text.replace(e2e_old, e2e_new, 1)
    TARGET.write_text(text)
    print("applied", TARGET.stat().st_size)

if __name__ == "__main__":
    main()
