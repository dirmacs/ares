use crate::{
    auth::middleware::AuthUser,
    research::coordinator::ResearchCoordinator,
    types::{ResearchRequest, ResearchResponse, Result, Source},
    utils::toml_config::{AresConfig, WorkflowConfig},
    AppState,
};
use axum::{extract::State, Json};
use std::time::Instant;

/// Resolves research depth and iteration limits from request overrides and workflow config.
fn resolve_research_limits(
    payload: &ResearchRequest,
    workflow: Option<&WorkflowConfig>,
) -> (u8, u8) {
    if let Some(workflow) = workflow {
        (
            payload.depth.unwrap_or(workflow.max_depth),
            payload.max_iterations.unwrap_or(workflow.max_iterations),
        )
    } else {
        (
            payload.depth.unwrap_or(2),
            payload.max_iterations.unwrap_or(5),
        )
    }
}

/// Returns the orchestrator agent model name, or a default when not configured.
fn orchestrator_model_name(config: &AresConfig) -> &str {
    config
        .get_agent("orchestrator")
        .map(|a| a.model.as_str())
        .unwrap_or("powerful")
}

/// Resolves depth, iteration cap, and orchestrator model without I/O (used by [`deep_research`]).
pub(crate) fn plan_research_run<'a>(
    config: &'a AresConfig,
    payload: &ResearchRequest,
) -> (u8, u8, &'a str) {
    let (depth, max_iterations) =
        resolve_research_limits(payload, config.get_workflow("research"));
    let model_name = orchestrator_model_name(config);
    (depth, max_iterations, model_name)
}

/// Builds the HTTP response body from coordinator output and elapsed time.
pub(crate) fn finalize_research_response(
    findings: String,
    sources: Vec<Source>,
    duration: std::time::Duration,
) -> ResearchResponse {
    ResearchResponse {
        findings,
        sources,
        duration_ms: duration.as_millis() as u64,
    }
}

/// Perform deep research on a query
#[utoipa::path(
    post,
    path = "/api/research",
    request_body = ResearchRequest,
    responses(
        (status = 200, description = "Research completed", body = ResearchResponse),
        (status = 400, description = "Invalid input"),
        (status = 401, description = "Unauthorized")
    ),
    tag = "research",
    security(("bearer" = []))
)]
pub async fn deep_research(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
    Json(payload): Json<ResearchRequest>,
) -> Result<Json<ResearchResponse>> {
    let start = Instant::now();

    let config = state.config_manager.config();
    let (depth, max_iterations, model_name) = plan_research_run(&config, &payload);

    let llm_client = match state
        .provider_registry
        .create_client_for_model(model_name)
        .await
    {
        Ok(client) => client,
        Err(_) => state.llm_factory.create_default().await?,
    };

    let coordinator = ResearchCoordinator::new(llm_client, depth, max_iterations);

    let (findings, sources) = coordinator.research(&payload.query).await?;

    let duration = start.elapsed();

    Ok(Json(finalize_research_response(
        findings, sources, duration,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::toml_config::{
        AgentConfig, AuthConfig, BillingConfig, DatabaseConfig, DynamicConfigPaths, RagConfig,
        ServerConfig,
    };
    use std::collections::HashMap;
    use std::time::Duration;

    fn minimal_ares_config(agents: HashMap<String, AgentConfig>) -> AresConfig {
        AresConfig {
            server: ServerConfig::default(),
            auth: AuthConfig::default(),
            database: DatabaseConfig::default(),
            nvidia: None,
            config: DynamicConfigPaths::default(),
            providers: HashMap::new(),
            models: HashMap::new(),
            tools: HashMap::new(),
            agents,
            workflows: HashMap::new(),
            rag: RagConfig::default(),
            billing: BillingConfig::default(),
            #[cfg(feature = "skills")]
            skills: None,
        }
    }

    fn sample_workflow() -> WorkflowConfig {
        WorkflowConfig {
            entry_agent: "orchestrator".to_string(),
            fallback_agent: None,
            max_depth: 4,
            max_iterations: 12,
            parallel_subagents: true,
        }
    }

    fn config_with_research_workflow(workflow: WorkflowConfig) -> AresConfig {
        let mut config = minimal_ares_config(HashMap::new());
        config
            .workflows
            .insert("research".to_string(), workflow);
        config
    }

    fn config_with_orchestrator(model: &str) -> AresConfig {
        let mut agents = HashMap::new();
        agents.insert(
            "orchestrator".to_string(),
            AgentConfig {
                model: model.to_string(),
                system_prompt: None,
                tools: vec![],
                max_tool_iterations: 5,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );
        minimal_ares_config(agents)
    }

    fn sample_request(query: &str) -> ResearchRequest {
        ResearchRequest {
            query: query.to_string(),
            depth: None,
            max_iterations: None,
        }
    }

    #[test]
    fn resolve_research_limits_uses_workflow_defaults() {
        let payload = sample_request("topic");
        let workflow = sample_workflow();
        assert_eq!(
            resolve_research_limits(&payload, Some(&workflow)),
            (4, 12)
        );
    }

    #[test]
    fn resolve_research_limits_honors_payload_overrides() {
        let payload = ResearchRequest {
            query: "topic".to_string(),
            depth: Some(1),
            max_iterations: Some(3),
        };
        assert_eq!(
            resolve_research_limits(&payload, Some(&sample_workflow())),
            (1, 3)
        );
    }

    #[test]
    fn resolve_research_limits_falls_back_when_workflow_missing() {
        let payload = sample_request("topic");
        assert_eq!(resolve_research_limits(&payload, None), (2, 5));
    }

    #[test]
    fn resolve_research_limits_partial_payload_override_depth_only() {
        let payload = ResearchRequest {
            query: "topic".to_string(),
            depth: Some(6),
            max_iterations: None,
        };
        assert_eq!(
            resolve_research_limits(&payload, Some(&sample_workflow())),
            (6, 12)
        );
    }

    #[test]
    fn resolve_research_limits_partial_payload_override_iterations_only() {
        let payload = ResearchRequest {
            query: "topic".to_string(),
            depth: None,
            max_iterations: Some(9),
        };
        assert_eq!(
            resolve_research_limits(&payload, Some(&sample_workflow())),
            (4, 9)
        );
    }

    #[test]
    fn resolve_research_limits_zero_payload_overrides() {
        let payload = ResearchRequest {
            query: "topic".to_string(),
            depth: Some(0),
            max_iterations: Some(0),
        };
        assert_eq!(
            resolve_research_limits(&payload, Some(&sample_workflow())),
            (0, 0)
        );
        assert_eq!(resolve_research_limits(&payload, None), (0, 0));
    }

    #[test]
    fn resolve_research_limits_u8_max_overrides() {
        let payload = ResearchRequest {
            query: "topic".to_string(),
            depth: Some(u8::MAX),
            max_iterations: Some(u8::MAX),
        };
        assert_eq!(
            resolve_research_limits(&payload, Some(&sample_workflow())),
            (u8::MAX, u8::MAX)
        );
    }

    #[test]
    fn resolve_research_limits_respects_workflow_zero_defaults() {
        let workflow = WorkflowConfig {
            max_depth: 0,
            max_iterations: 0,
            ..sample_workflow()
        };
        let payload = sample_request("topic");
        assert_eq!(
            resolve_research_limits(&payload, Some(&workflow)),
            (0, 0)
        );
    }

    #[test]
    fn orchestrator_model_name_reads_configured_agent() {
        let config = config_with_orchestrator("claude-research");
        assert_eq!(orchestrator_model_name(&config), "claude-research");
    }

    #[test]
    fn orchestrator_model_name_defaults_without_agent() {
        let config = minimal_ares_config(HashMap::new());
        assert_eq!(orchestrator_model_name(&config), "powerful");
    }

    #[test]
    fn orchestrator_model_name_ignores_non_orchestrator_agents() {
        let mut agents = HashMap::new();
        agents.insert(
            "researcher".to_string(),
            AgentConfig {
                model: "other-model".to_string(),
                system_prompt: None,
                tools: vec![],
                max_tool_iterations: 1,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );
        let config = minimal_ares_config(agents);
        assert_eq!(orchestrator_model_name(&config), "powerful");
    }

    #[test]
    fn orchestrator_model_name_case_sensitive_agent_key() {
        let mut agents = HashMap::new();
        agents.insert(
            "Orchestrator".to_string(),
            AgentConfig {
                model: "wrong-case".to_string(),
                system_prompt: None,
                tools: vec![],
                max_tool_iterations: 1,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );
        let config = minimal_ares_config(agents);
        assert_eq!(orchestrator_model_name(&config), "powerful");
    }

    #[test]
    fn orchestrator_model_name_preserves_empty_model_string() {
        let config = config_with_orchestrator("");
        assert_eq!(orchestrator_model_name(&config), "");
    }

    #[test]
    fn orchestrator_model_name_does_not_read_llm_env() {
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("OLLAMA_URL");
        let config = config_with_orchestrator("local-model");
        assert_eq!(orchestrator_model_name(&config), "local-model");
    }

    #[test]
    fn plan_research_run_uses_research_workflow_from_config() {
        let config = config_with_research_workflow(sample_workflow());
        let payload = sample_request("climate");
        assert_eq!(plan_research_run(&config, &payload), (4, 12, "powerful"));
    }

    #[test]
    fn plan_research_run_combines_overrides_and_orchestrator_model() {
        let mut config = config_with_orchestrator("gpt-research");
        config
            .workflows
            .insert("research".to_string(), sample_workflow());
        let payload = ResearchRequest {
            query: "topic".to_string(),
            depth: Some(2),
            max_iterations: Some(8),
        };
        assert_eq!(
            plan_research_run(&config, &payload),
            (2, 8, "gpt-research")
        );
    }

    #[test]
    fn plan_research_run_without_workflow_uses_hardcoded_defaults() {
        let config = config_with_orchestrator("fast");
        let payload = sample_request("topic");
        assert_eq!(plan_research_run(&config, &payload), (2, 5, "fast"));
    }

    #[test]
    fn finalize_research_response_maps_duration_to_millis() {
        let response = finalize_research_response(
            "done".to_string(),
            vec![],
            Duration::from_millis(1500),
        );
        assert_eq!(response.findings, "done");
        assert!(response.sources.is_empty());
        assert_eq!(response.duration_ms, 1500);
    }

    #[test]
    fn finalize_research_response_truncates_sub_millisecond_duration() {
        let response = finalize_research_response(
            "fast".to_string(),
            vec![],
            Duration::from_nanos(999_999),
        );
        assert_eq!(response.duration_ms, 0);
    }

    #[test]
    fn finalize_research_response_preserves_sources() {
        let sources = vec![Source {
            title: "Doc".to_string(),
            url: None,
            relevance_score: 0.5,
        }];
        let response =
            finalize_research_response("x".into(), sources.clone(), Duration::ZERO);
        assert_eq!(response.sources.len(), 1);
        assert_eq!(response.sources[0].title, "Doc");
        assert!(response.sources[0].url.is_none());
    }

    #[test]
    fn research_response_serde_roundtrip() {
        let response = ResearchResponse {
            findings: "Summary".to_string(),
            sources: vec![Source {
                title: "Paper".to_string(),
                url: Some("https://example.com".to_string()),
                relevance_score: 0.9,
            }],
            duration_ms: 42,
        };
        let json = serde_json::to_string(&response).unwrap();
        let parsed: ResearchResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.findings, "Summary");
        assert_eq!(parsed.sources.len(), 1);
        assert_eq!(parsed.duration_ms, 42);
    }

    #[test]
    fn research_response_serde_empty_sources() {
        let response = ResearchResponse {
            findings: String::new(),
            sources: vec![],
            duration_ms: 0,
        };
        let json = serde_json::to_string(&response).unwrap();
        let parsed: ResearchResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.sources.is_empty());
        assert_eq!(parsed.findings, "");
    }

    #[test]
    fn research_request_serde_roundtrip() {
        let req = ResearchRequest {
            query: "rust async".to_string(),
            depth: Some(2),
            max_iterations: Some(10),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: ResearchRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.query, req.query);
        assert_eq!(parsed.depth, req.depth);
        assert_eq!(parsed.max_iterations, req.max_iterations);
    }

    #[test]
    fn research_request_debug_includes_query() {
        let req = sample_request("debug me");
        let debug = format!("{req:?}");
        assert!(debug.contains("debug me"));
    }

    #[test]
    fn research_response_debug_includes_duration() {
        let response = ResearchResponse {
            findings: "f".into(),
            sources: vec![],
            duration_ms: 99,
        };
        let debug = format!("{response:?}");
        assert!(debug.contains("99"));
    }

    #[test]
    fn research_request_deserializes_query_only() {
        let req: ResearchRequest =
            serde_json::from_str(r#"{"query":"quantum computing"}"#).unwrap();
        assert_eq!(req.query, "quantum computing");
        assert!(req.depth.is_none());
        assert!(req.max_iterations.is_none());
    }

    #[test]
    fn research_request_deserializes_with_overrides() {
        let req: ResearchRequest = serde_json::from_str(
            r#"{"query":"ai safety","depth":3,"max_iterations":7}"#,
        )
        .unwrap();
        assert_eq!(req.depth, Some(3));
        assert_eq!(req.max_iterations, Some(7));
    }

    #[test]
    fn research_request_deserializes_explicit_null_overrides() {
        let req: ResearchRequest = serde_json::from_str(
            r#"{"query":"topic","depth":null,"max_iterations":null}"#,
        )
        .unwrap();
        assert!(req.depth.is_none());
        assert!(req.max_iterations.is_none());
    }

    #[test]
    fn research_request_rejects_missing_query() {
        let err = serde_json::from_str::<ResearchRequest>(r#"{}"#).unwrap_err();
        assert!(err.to_string().contains("query"));
    }
}
