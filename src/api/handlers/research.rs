use crate::{
    auth::middleware::AuthUser,
    research::coordinator::ResearchCoordinator,
    types::{ResearchRequest, ResearchResponse, Result},
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
    let (depth, max_iterations) =
        resolve_research_limits(&payload, config.get_workflow("research"));

    let model_name = orchestrator_model_name(&config);

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

    Ok(Json(ResearchResponse {
        findings,
        sources,
        duration_ms: duration.as_millis() as u64,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Source;
    use crate::utils::toml_config::{
        AgentConfig, AuthConfig, BillingConfig, DatabaseConfig, DynamicConfigPaths, RagConfig,
        ServerConfig,
    };
    use std::collections::HashMap;

    fn minimal_ares_config(agents: HashMap<String, AgentConfig>) -> AresConfig {
        AresConfig {
            server: ServerConfig::default(),
            auth: AuthConfig::default(),
            database: DatabaseConfig::default(),
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

    #[test]
    fn resolve_research_limits_uses_workflow_defaults() {
        let payload = ResearchRequest {
            query: "topic".to_string(),
            depth: None,
            max_iterations: None,
        };
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
        let payload = ResearchRequest {
            query: "topic".to_string(),
            depth: None,
            max_iterations: None,
        };
        assert_eq!(resolve_research_limits(&payload, None), (2, 5));
    }

    #[test]
    fn resolve_research_limits_partial_payload_override() {
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
    fn orchestrator_model_name_does_not_read_llm_env() {
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("OLLAMA_URL");
        let config = config_with_orchestrator("local-model");
        assert_eq!(orchestrator_model_name(&config), "local-model");
    }
}
