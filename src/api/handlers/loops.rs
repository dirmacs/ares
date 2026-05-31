//! HTTP handlers for loop-mode agent lifecycle management.
//!
//! Exposes three endpoints:
//!
//! - `POST /loops/start`   — start a new LoopRunner for a named agent
//! - `GET  /loops`         — list all registered loops with state summaries
//! - `DELETE /loops/{id}`  — signal a running loop to stop

use crate::agents::loop_mode::{LoopFinishReason, LoopModeConfig, LoopModeState, LoopRunner};
use crate::types::{AppError, Result};
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Registry types
// ---------------------------------------------------------------------------

/// In-memory entry for a single running (or finished) loop.
pub struct LoopEntry {
    pub id: String,
    pub agent_name: String,
    pub config: LoopModeConfig,
    pub state: LoopModeState,
    pub stop: Arc<AtomicBool>,
    pub started_at: u64,
    pub finish_reason: Option<LoopFinishReason>,
}

/// Thread-safe registry of all loop entries.
#[derive(Clone, Default)]
pub struct LoopRegistry {
    entries: Arc<Mutex<HashMap<String, LoopEntry>>>,
}

impl LoopRegistry {
    pub fn new() -> Self {
        Self::default()
    }
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StartLoopRequest {
    /// Name of the agent (tenant agent name) to tick.
    pub agent: String,
    /// Loop scheduling and halt configuration.
    #[serde(default)]
    pub config: LoopModeConfig,
    /// Prompt passed to the agent on every tick.
    #[serde(default)]
    pub prompt: String,
}

#[derive(Debug, Serialize)]
pub struct StartLoopResponse {
    pub id: String,
}

/// JSON-serialisable summary of a loop entry (omits the non-serialisable stop handle).
#[derive(Debug, Serialize)]
pub struct LoopSummary {
    pub id: String,
    pub agent_name: String,
    pub config: LoopModeConfig,
    pub state: LoopModeState,
    pub started_at: u64,
    pub finish_reason: Option<LoopFinishReason>,
    /// True while the loop task is still running (no finish_reason yet).
    pub running: bool,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /loops/start — spawn a new LoopRunner task and return its ID.
pub async fn start_loop(
    State(state): State<AppState>,
    Json(req): Json<StartLoopRequest>,
) -> Result<Json<StartLoopResponse>> {
    let id = Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Build the entry and grab the stop handle before moving into the task.
    let mut runner = LoopRunner::new(req.config.clone());
    let stop_handle = runner.stop_handle();

    let entry = LoopEntry {
        id: id.clone(),
        agent_name: req.agent.clone(),
        config: req.config.clone(),
        state: LoopModeState::default(),
        stop: stop_handle.clone(),
        started_at: now,
        finish_reason: None,
    };

    state.loop_registry.entries.lock().await.insert(id.clone(), entry);

    // Clone handles needed by the background task.
    let registry = state.loop_registry.clone();
    let loop_id = id.clone();
    let agent_name = req.agent.clone();
    let prompt = req.prompt.clone();

    tokio::spawn(async move {
        // No-op tick: logs the agent name and prompt, then returns Ok.
        // Future phases will call agent.execute() here once the orchestrator
        // is wired up to LoopRunner.
        let tick = build_tick(agent_name, prompt);
        let finish_reason = runner.run(&tick).await;

        // Write back the final state and finish reason.
        let mut entries = registry.entries.lock().await;
        if let Some(e) = entries.get_mut(&loop_id) {
            e.state = runner.state.clone();
            e.finish_reason = Some(finish_reason);
        }
    });

    Ok(Json(StartLoopResponse { id }))
}

/// GET /loops — list all loops with their current state summaries.
pub async fn list_loops(State(state): State<AppState>) -> Json<Vec<LoopSummary>> {
    let entries = state.loop_registry.entries.lock().await;
    let mut list: Vec<LoopSummary> = entries
        .values()
        .map(|e| LoopSummary {
            id: e.id.clone(),
            agent_name: e.agent_name.clone(),
            config: e.config.clone(),
            state: e.state.clone(),
            started_at: e.started_at,
            finish_reason: e.finish_reason.clone(),
            running: e.finish_reason.is_none() && !e.stop.load(Ordering::Relaxed),
        })
        .collect();
    // Newest first.
    list.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    Json(list)
}

/// DELETE /loops/{id} — set the stop flag; the runner will halt after the current tick.
pub async fn stop_loop(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let entries = state.loop_registry.entries.lock().await;
    match entries.get(&id) {
        Some(e) => {
            e.stop.store(true, Ordering::Relaxed);
            Ok(StatusCode::NO_CONTENT)
        }
        None => Err(AppError::NotFound(format!("Loop '{}' not found", id))),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build a no-op tick function that logs agent name and prompt.
/// Returns Ok(()) immediately — real agent dispatch lands in a future phase.
fn build_tick(
    agent_name: String,
    prompt: String,
) -> crate::agents::loop_mode::TickFn {
    Box::new(move || {
        let agent = agent_name.clone();
        let p = prompt.clone();
        Box::pin(async move {
            tracing::debug!(agent = %agent, prompt = %p, "loop tick (no-op)");
            Ok(())
        })
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::loop_mode::{LoopFinishReason, LoopModeConfig, LoopModeState};

    // Helper: insert a dummy entry into a registry.
    async fn insert_dummy(registry: &LoopRegistry, id: &str) -> Arc<AtomicBool> {
        let stop = Arc::new(AtomicBool::new(false));
        let entry = LoopEntry {
            id: id.to_string(),
            agent_name: "test-agent".to_string(),
            config: LoopModeConfig::default(),
            state: LoopModeState::default(),
            stop: stop.clone(),
            started_at: 1_000_000,
            finish_reason: None,
        };
        registry.entries.lock().await.insert(id.to_string(), entry);
        stop
    }

    #[tokio::test]
    async fn registry_insert_and_list() {
        let registry = LoopRegistry::new();
        insert_dummy(&registry, "loop-1").await;
        insert_dummy(&registry, "loop-2").await;

        let entries = registry.entries.lock().await;
        assert_eq!(entries.len(), 2);
        assert!(entries.contains_key("loop-1"));
        assert!(entries.contains_key("loop-2"));
    }

    #[tokio::test]
    async fn registry_stop_sets_flag() {
        let registry = LoopRegistry::new();
        let stop = insert_dummy(&registry, "loop-stop").await;

        assert!(!stop.load(Ordering::Relaxed));
        {
            let entries = registry.entries.lock().await;
            entries
                .get("loop-stop")
                .unwrap()
                .stop
                .store(true, Ordering::Relaxed);
        }
        assert!(stop.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn registry_finish_reason_transition() {
        let registry = LoopRegistry::new();
        insert_dummy(&registry, "loop-fin").await;

        // Simulate loop completing.
        {
            let mut entries = registry.entries.lock().await;
            let e = entries.get_mut("loop-fin").unwrap();
            e.finish_reason = Some(LoopFinishReason::MaxIterationsReached);
        }

        let entries = registry.entries.lock().await;
        assert_eq!(
            entries.get("loop-fin").unwrap().finish_reason,
            Some(LoopFinishReason::MaxIterationsReached)
        );
    }

    #[tokio::test]
    async fn registry_running_flag_reflects_state() {
        let registry = LoopRegistry::new();
        let stop = insert_dummy(&registry, "loop-run").await;

        // Should be running.
        {
            let entries = registry.entries.lock().await;
            let e = entries.get("loop-run").unwrap();
            let running = e.finish_reason.is_none() && !e.stop.load(Ordering::Relaxed);
            assert!(running);
        }

        // After stop flag is set, running = false.
        stop.store(true, Ordering::Relaxed);
        {
            let entries = registry.entries.lock().await;
            let e = entries.get("loop-run").unwrap();
            let running = e.finish_reason.is_none() && !e.stop.load(Ordering::Relaxed);
            assert!(!running);
        }

    #[test]
    fn start_loop_request_deserializes_defaults() {
        let req: StartLoopRequest =
            serde_json::from_str(r#"{"agent":"support"}"#).unwrap();
        assert_eq!(req.agent, "support");
        assert_eq!(req.config, LoopModeConfig::default());
        assert!(req.prompt.is_empty());
    }

    #[test]
    fn loop_summary_serializes_running_flag() {
        let summary = LoopSummary {
            id: "id-1".into(),
            agent_name: "agent".into(),
            config: LoopModeConfig::default(),
            state: LoopModeState::default(),
            started_at: 42,
            finish_reason: None,
            running: true,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"running\":true"));
    }

    #[tokio::test]
    async fn build_tick_executes_successfully() {
        let tick = build_tick("agent".into(), "prompt".into());
        assert!(tick().await.is_ok());
    }

    #[cfg(feature = "postgres")]
    mod handler_tests {
        use super::*;
        use crate::agents::context_provider::NoOpContextProvider;
        use crate::agents::loop_mode::LoopModeConfig;
        use crate::auth::jwt::AuthService;
        use crate::db::{PostgresClient, TenantDb};
        use crate::utils::toml_config::{
            AgentConfig, AresConfig, AuthConfig, BillingConfig, DatabaseConfig,
            DynamicConfigPaths, ModelConfig, ProviderConfig, RagConfig, ServerConfig,
        };
        use crate::{
            AgentRegistry, AppState, AresConfigManager, ConfigBasedLLMFactory,
            DynamicConfigManager, ProviderRegistry, ToolRegistry,
        };
        use axum::extract::{Path, State};
        use std::collections::HashMap;
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        use std::time::Duration;

        fn minimal_config() -> AresConfig {
            let mut providers = HashMap::new();
            providers.insert(
                "p".into(),
                ProviderConfig::Ollama {
                    base_url: "http://127.0.0.1:11434".into(),
                    default_model: "m".into(),
                },
            );
            let mut models = HashMap::new();
            models.insert(
                "default".into(),
                ModelConfig {
                    provider: "p".into(),
                    model: "m".into(),
                    temperature: 0.7,
                    max_tokens: 512,
                    top_p: None,
                    frequency_penalty: None,
                    presence_penalty: None,
                },
            );
            let mut agents = HashMap::new();
            agents.insert(
                "a".into(),
                AgentConfig {
                    model: "default".into(),
                    system_prompt: None,
                    tools: vec![],
                    max_tool_iterations: 1,
                    parallel_tools: false,
                    extra: HashMap::new(),
                },
            );
            AresConfig {
                server: ServerConfig::default(),
                auth: AuthConfig {
                    jwt_secret_env: "JWT_SECRET".into(),
                    jwt_access_expiry: 900,
                    jwt_refresh_expiry: 604800,
                    api_key_env: "API_KEY".into(),
                },
                database: DatabaseConfig::default(),
                config: DynamicConfigPaths::default(),
                providers,
                models,
                tools: HashMap::new(),
                agents,
                workflows: HashMap::new(),
                rag: RagConfig::default(),
                billing: BillingConfig::default(),
                #[cfg(feature = "skills")]
                skills: None,
            }
        }

        fn test_app_state() -> AppState {
            let config = minimal_config();
            let config_manager = Arc::new(AresConfigManager::from_config(config));
            let provider_registry =
                Arc::new(ProviderRegistry::from_config(&config_manager.config()));
            let tool_registry = Arc::new(ToolRegistry::new());
            let agent_registry = Arc::new(AgentRegistry::from_config(
                &config_manager.config(),
                provider_registry.clone(),
                tool_registry.clone(),
            ));
            let temp_dir = tempfile::tempdir().expect("tempdir");
            let base = temp_dir.path();
            for sub in ["agents", "models", "tools", "workflows", "mcps"] {
                std::fs::create_dir_all(base.join(sub)).expect("mkdir");
            }
            let dynamic_config = Arc::new(
                DynamicConfigManager::new(
                    base.join("agents"),
                    base.join("models"),
                    base.join("tools"),
                    base.join("workflows"),
                    base.join("mcps"),
                    false,
                )
                .expect("dynamic config"),
            );
            std::mem::forget(temp_dir);

            let db = Arc::new(PostgresClient::new_test());
            AppState {
                config_manager,
                dynamic_config,
                db: db.clone(),
                tenant_db: Arc::new(TenantDb::new(db)),
                llm_factory: Arc::new(ConfigBasedLLMFactory::new(
                    provider_registry.clone(),
                    "default",
                )),
                provider_registry,
                agent_registry,
                tool_registry,
                auth_service: Arc::new(AuthService::new(
                    "test-secret-at-least-32-characters-long".into(),
                    900,
                    604800,
                )),
                deploy_registry: crate::api::handlers::deploy::new_deploy_registry(),
                loop_registry: LoopRegistry::new(),
                emergency_stop: Arc::new(AtomicBool::new(false)),
                context_provider: Arc::new(NoOpContextProvider),
                #[cfg(feature = "mcp")]
                mcp_registry: None,
            }
        }

        #[tokio::test]
        async fn start_loop_registers_entry() {
            let state = test_app_state();
            let config = LoopModeConfig {
                interval_secs: 0,
                max_iterations: Some(1),
                ..LoopModeConfig::default()
            };
            let resp = start_loop(
                State(state.clone()),
                Json(StartLoopRequest {
                    agent: "test-agent".into(),
                    config,
                    prompt: "tick".into(),
                }),
            )
            .await
            .expect("start");
            tokio::time::sleep(Duration::from_millis(100)).await;
            let list = list_loops(State(state)).await.0;
            assert!(list.iter().any(|l| l.id == resp.0.id));
        }

        #[tokio::test]
        async fn stop_loop_sets_stop_flag_for_existing_loop() {
            let state = test_app_state();
            let resp = start_loop(
                State(state.clone()),
                Json(StartLoopRequest {
                    agent: "agent".into(),
                    config: LoopModeConfig {
                        interval_secs: 60,
                        max_iterations: None,
                        ..LoopModeConfig::default()
                    },
                    prompt: String::new(),
                }),
            )
            .await
            .expect("start");
            let status = stop_loop(State(state), Path(resp.0.id)).await.expect("stop");
            assert_eq!(status, StatusCode::NO_CONTENT);
        }

        #[tokio::test]
        async fn stop_loop_returns_not_found_for_missing_id() {
            let state = test_app_state();
            let err = stop_loop(State(state), Path("missing".into()))
                .await
                .unwrap_err();
            assert!(matches!(err, AppError::NotFound(_)));
        }

        #[tokio::test]
        async fn list_loops_marks_stopped_loop_not_running() {
            let registry = LoopRegistry::new();
            let stop = insert_dummy(&registry, "loop-run").await;
            let state = test_app_state();
            // Replace default registry with our prepared one
            let mut state = state;
            state.loop_registry = registry;
            stop.store(true, Ordering::Relaxed);
            let list = list_loops(State(state)).await.0;
            let entry = list.iter().find(|l| l.id == "loop-run").expect("entry");
            assert!(!entry.running);
        }
    }
    }
}
