
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

