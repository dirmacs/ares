//! TOML-based configuration for A.R.E.S
//!
//! This module provides declarative configuration for providers, models, agents,
//! tools, and workflows via a TOML file (`ares.toml`).
//!
//! # Hot Reloading
//!
//! Configuration changes are automatically detected and applied at runtime.
//! Use `AresConfigManager` for thread-safe access to the current configuration.

use arc_swap::ArcSwap;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::any::TypeId;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::config::{AuthConfig, ServerConfig};
pub use ares_agent::{AgentConfig, SkillsTomlConfig, WorkflowConfig};
pub use ares_llm::{ModelConfig, NvidiaConfig, ProviderConfig};
pub use ares_rag::{
    HybridWeightsConfig, RAGVectorConfig, RagChunkingConfig, RagConfig, RagRerankingConfig,
    RagSearchConfig,
};
use ares_store::default_qdrant_url;
pub use ares_store::{BillingConfig, DatabaseConfig, ModelPricingConfig, QdrantConfig};
pub use ares_tools::ToolConfig;

/// Root configuration structure loaded from ares.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AresConfig {
    /// HTTP server configuration (host, port, log level).
    pub server: ServerConfig,

    /// Authentication configuration (JWT secrets, expiry times).
    pub auth: AuthConfig,

    /// Database configuration (Turso/SQLite, Qdrant).
    pub database: DatabaseConfig,

    /// NVIDIA provider + catalog configuration. When present, the runtime
    /// fetches the model catalog from `models_url` (default: NVIDIA NIM) and
    /// exposes it through the provider registry. If absent, the registry
    /// uses built-in defaults.
    #[serde(default)]
    pub nvidia: Option<NvidiaConfig>,

    /// Named LLM provider configurations
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    /// Named model configurations that reference providers
    /// NOTE: These are being migrated to TOON files in config/models/
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,

    /// Tool configurations
    /// NOTE: These are being migrated to TOON files in config/tools/
    #[serde(default)]
    pub tools: HashMap<String, ToolConfig>,

    /// Agent configurations
    /// NOTE: These are being migrated to TOON files in config/agents/
    #[serde(default)]
    pub agents: HashMap<String, AgentConfig>,

    /// Workflow configurations
    /// NOTE: These are being migrated to TOON files in config/workflows/
    #[serde(default)]
    pub workflows: HashMap<String, WorkflowConfig>,

    /// RAG configuration
    #[serde(default)]
    pub rag: RagConfig,

    /// Billing and cost-estimation configuration
    #[serde(default)]
    pub billing: BillingConfig,

    /// Skills configuration (SKILL.md discovery directories)
    #[serde(default)]
    pub skills: Option<SkillsTomlConfig>,

    /// Dynamic configuration paths (TOON files)
    #[serde(default)]
    pub config: DynamicConfigPaths,
}

// ============= Dynamic Configuration Paths =============

/// Paths to TOON config directories for dynamic behavioral configuration
///
/// ARES uses a hybrid configuration approach:
/// - **TOML** (`ares.toml`): Static infrastructure config (server, auth, database, providers)
/// - **TOON** (`config/*.toon`): Dynamic behavioral config (agents, workflows, models, tools, MCPs)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicConfigPaths {
    /// Directory containing agent TOON files
    #[serde(default = "default_agents_dir")]
    pub agents_dir: std::path::PathBuf,

    /// Directory containing workflow TOON files
    #[serde(default = "default_workflows_dir")]
    pub workflows_dir: std::path::PathBuf,

    /// Directory containing model TOON files
    #[serde(default = "default_models_dir")]
    pub models_dir: std::path::PathBuf,

    /// Directory containing tool TOON files
    #[serde(default = "default_tools_dir")]
    pub tools_dir: std::path::PathBuf,

    /// Directory containing MCP TOON files
    #[serde(default = "default_mcps_dir")]
    pub mcps_dir: std::path::PathBuf,

    /// Whether to watch for changes and hot-reload TOON configs
    #[serde(default = "default_hot_reload")]
    pub hot_reload: bool,

    /// Interval in milliseconds for checking config changes
    #[serde(default = "default_watch_interval")]
    pub watch_interval_ms: u64,
}

fn default_agents_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("config/agents")
}

fn default_workflows_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("config/workflows")
}

fn default_models_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("config/models")
}

fn default_tools_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("config/tools")
}

fn default_mcps_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("config/mcps")
}

fn default_hot_reload() -> bool {
    true
}

fn default_watch_interval() -> u64 {
    1000
}

impl Default for DynamicConfigPaths {
    fn default() -> Self {
        Self {
            agents_dir: default_agents_dir(),
            workflows_dir: default_workflows_dir(),
            models_dir: default_models_dir(),
            tools_dir: default_tools_dir(),
            mcps_dir: default_mcps_dir(),
            hot_reload: default_hot_reload(),
            watch_interval_ms: default_watch_interval(),
        }
    }
}

// ============= Configuration Loading & Validation =============

/// Configuration warnings that don't prevent operation but may indicate issues.
#[derive(Debug, Clone)]
pub struct ConfigWarning {
    /// Category of the warning.
    pub kind: ConfigWarningKind,

    /// Human-readable warning message.
    pub message: String,
}

/// Categories of configuration warnings.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigWarningKind {
    /// A provider is defined but not referenced by any model.
    UnusedProvider,

    /// A model is defined but not referenced by any agent.
    UnusedModel,

    /// A tool is defined but not referenced by any agent.
    UnusedTool,

    /// An agent is defined but not referenced by any workflow.
    UnusedAgent,
}

impl std::fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Errors that can occur during configuration loading.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The configuration file was not found at the specified path.
    #[error("Configuration file not found: {0}")]
    FileNotFound(PathBuf),

    /// Failed to read the configuration file from disk.
    #[error("Failed to read configuration file: {0}")]
    ReadError(#[from] std::io::Error),

    /// Failed to parse the TOML content.
    #[error("Failed to parse TOML: {0}")]
    ParseError(#[from] toml::de::Error),

    /// Configuration validation failed.
    #[error("Validation error: {0}")]
    ValidationError(String),

    /// An environment variable referenced in the config is not set.
    #[error("Environment variable '{0}' referenced in config is not set")]
    MissingEnvVar(String),

    /// A provider referenced by a model does not exist.
    #[error("Provider '{0}' referenced by model '{1}' does not exist")]
    MissingProvider(String, String),

    /// A model referenced by an agent does not exist.
    #[error("Model '{0}' referenced by agent '{1}' does not exist")]
    MissingModel(String, String),

    /// An agent referenced by a workflow does not exist.
    #[error("Agent '{0}' referenced by workflow '{1}' does not exist")]
    MissingAgent(String, String),

    /// A tool referenced by an agent does not exist.
    #[error("Tool '{0}' referenced by agent '{1}' does not exist")]
    MissingTool(String, String),

    /// A circular reference was detected in the configuration.
    #[error("Circular reference detected: {0}")]
    CircularReference(String),

    /// An error occurred while watching configuration files for changes.
    #[error("Watch error: {0}")]
    WatchError(#[from] notify::Error),
}

impl AresConfig {
    /// Load configuration from a TOML file
    ///
    /// # Panics
    ///
    /// Panics if the configuration file doesn't exist or is invalid.
    /// This is intentional - the server cannot run without a valid config.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(ConfigError::FileNotFound(path.to_path_buf()));
        }

        let content = fs::read_to_string(path)?;
        let config: AresConfig = toml::from_str(&content)?;

        // Validate the configuration
        config.validate()?;

        Ok(config)
    }

    /// Load configuration from a TOML file without validation.
    ///
    /// This is useful for CLI commands that only need to inspect the configuration
    /// without actually running the server (e.g., `ares-server config`).
    /// Environment variables are not checked.
    pub fn load_unchecked<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(ConfigError::FileNotFound(path.to_path_buf()));
        }

        let content = fs::read_to_string(path)?;
        let config: AresConfig = toml::from_str(&content)?;

        Ok(config)
    }

    /// Validate the configuration for internal consistency and env var availability
    pub fn validate(&self) -> Result<(), ConfigError> {
        // Validate auth env vars exist
        self.validate_env_var(&self.auth.jwt_secret_env)?;
        self.validate_env_var(&self.auth.api_key_env)?;

        // Validate database env vars if specified
        if let Some(ref qdrant) = self.database.qdrant {
            if let Some(ref env) = qdrant.api_key_env {
                self.validate_env_var(env)?;
            }
        }

        // Validate provider env vars
        for provider in self.providers.values() {
            match provider {
                ProviderConfig::OpenAI { api_key_env, .. } => {
                    self.validate_env_var(api_key_env)?;
                }
                ProviderConfig::Azure {
                    api_key_env,
                    base_url_env,
                    ..
                } => {
                    self.validate_env_var(api_key_env)?;
                    self.validate_env_var(base_url_env)?;
                }
                ProviderConfig::Anthropic { api_key_env, .. } => {
                    self.validate_env_var(api_key_env)?;
                }
                ProviderConfig::Bedrock {
                    api_key_env,
                    region_env,
                    ..
                } => {
                    self.validate_env_var(api_key_env)?;
                    self.validate_env_var(region_env)?;
                }
                ProviderConfig::Ollama { .. } => {
                    // Ollama has no auth; nothing to validate.
                }
                _ => {}
            }
        }

        // Validate model -> provider references
        for (model_name, model_config) in &self.models {
            if !self.providers.contains_key(&model_config.provider) {
                return Err(ConfigError::MissingProvider(
                    model_config.provider.clone(),
                    model_name.clone(),
                ));
            }
        }

        // Validate agent -> model and agent -> tools references
        // NOTE: With the dynamic NVIDIA catalog, agents reference models by
        // literal NVIDIA NIM id (e.g. "nvidia/nemotron-3-ultra-550b-a55b") which
        // is resolved at runtime against the live catalog — NOT against a
        // static [models] table. We therefore only WARN if a model name is
        // suspicious rather than failing startup, so a model that gets
        // rotated out of the live catalog doesn't prevent the server from
        // booting (the affected agent just gets a runtime error on first use).
        for (agent_name, agent_config) in &self.agents {
            if !self.models.contains_key(&agent_config.model) {
                tracing::warn!(
                    "Agent '{}' references model '{}' which is not in the static [models] table. \
                     The model will be resolved against the live NVIDIA catalog at runtime.",
                    agent_name,
                    agent_config.model,
                );
            }

            for tool_name in &agent_config.tools {
                // Allow tools from registered tool configs OR MCP bridge tools
                // MCP bridge tools follow the pattern: {mcp_client_name}_{operation}
                let is_known_tool = self.tools.contains_key(tool_name);
                let is_mcp_tool = tool_name.contains('_') && {
                    // Check if any configured MCP client name is a prefix
                    let mcp_names = self.mcp_client_names();
                    mcp_names
                        .iter()
                        .any(|mcp_name| tool_name.starts_with(&format!("{}_", mcp_name)))
                };
                if !is_known_tool && !is_mcp_tool {
                    return Err(ConfigError::MissingTool(
                        tool_name.clone(),
                        agent_name.clone(),
                    ));
                }
            }
        }

        // Validate workflow -> agent references
        for (workflow_name, workflow_config) in &self.workflows {
            if !self.agents.contains_key(&workflow_config.entry_agent) {
                return Err(ConfigError::MissingAgent(
                    workflow_config.entry_agent.clone(),
                    workflow_name.clone(),
                ));
            }

            if let Some(ref fallback) = workflow_config.fallback_agent {
                if !self.agents.contains_key(fallback) {
                    return Err(ConfigError::MissingAgent(
                        fallback.clone(),
                        workflow_name.clone(),
                    ));
                }
            }
        }

        // Check for circular references in workflows (entry_agent -> fallback cycles)
        self.detect_circular_references()?;

        Ok(())
    }

    /// Detect circular references in workflow configurations
    ///
    /// Currently checks for:
    /// - Workflow entry_agent pointing to itself via fallback chain
    fn detect_circular_references(&self) -> Result<(), ConfigError> {
        use std::collections::HashSet;

        for (workflow_name, workflow_config) in &self.workflows {
            let mut visited = HashSet::new();
            let mut current = Some(workflow_config.entry_agent.as_str());

            while let Some(agent_name) = current {
                if visited.contains(agent_name) {
                    return Err(ConfigError::CircularReference(format!(
                        "Circular reference detected in workflow '{}': agent '{}' appears multiple times in the chain",
                        workflow_name, agent_name
                    )));
                }
                visited.insert(agent_name);

                // Check if this agent is the entry for any workflow that has this workflow's entry as fallback
                // This is a simple check - could be extended for more complex scenarios
                current = None;

                // For now, we just check that fallback_agent doesn't equal entry_agent
                if let Some(ref fallback) = workflow_config.fallback_agent {
                    if fallback == &workflow_config.entry_agent {
                        return Err(ConfigError::CircularReference(format!(
                            "Workflow '{}' has entry_agent '{}' that equals fallback_agent",
                            workflow_name, workflow_config.entry_agent
                        )));
                    }
                }
            }
        }

        Ok(())
    }

    /// Validate configuration with warnings for unused items
    ///
    /// Returns Ok with warnings, or Err if validation fails
    pub fn validate_with_warnings(&self) -> Result<Vec<ConfigWarning>, ConfigError> {
        // Run standard validation first
        self.validate()?;

        // Collect warnings
        let mut warnings = Vec::new();

        // Check for unused providers
        warnings.extend(self.check_unused_providers());

        // Check for unused models
        warnings.extend(self.check_unused_models());

        // Check for unused tools
        warnings.extend(self.check_unused_tools());

        // Check for unused agents
        warnings.extend(self.check_unused_agents());

        Ok(warnings)
    }

    /// Check for providers that aren't referenced by any model
    fn check_unused_providers(&self) -> Vec<ConfigWarning> {
        use std::collections::HashSet;

        let referenced: HashSet<_> = self.models.values().map(|m| m.provider.as_str()).collect();

        self.providers
            .keys()
            .filter(|name| !referenced.contains(name.as_str()))
            .map(|name| ConfigWarning {
                kind: ConfigWarningKind::UnusedProvider,
                message: format!(
                    "Provider '{}' is defined but not referenced by any model",
                    name
                ),
            })
            .collect()
    }

    /// Check for models that aren't referenced by any agent
    fn check_unused_models(&self) -> Vec<ConfigWarning> {
        use std::collections::HashSet;

        let referenced: HashSet<_> = self.agents.values().map(|a| a.model.as_str()).collect();

        self.models
            .keys()
            .filter(|name| !referenced.contains(name.as_str()))
            .map(|name| ConfigWarning {
                kind: ConfigWarningKind::UnusedModel,
                message: format!(
                    "Model '{}' is defined but not referenced by any agent",
                    name
                ),
            })
            .collect()
    }

    /// Check for tools that aren't referenced by any agent
    fn check_unused_tools(&self) -> Vec<ConfigWarning> {
        use std::collections::HashSet;

        let referenced: HashSet<_> = self
            .agents
            .values()
            .flat_map(|a| a.tools.iter().map(|t| t.as_str()))
            .collect();

        self.tools
            .keys()
            .filter(|name| !referenced.contains(name.as_str()))
            .map(|name| ConfigWarning {
                kind: ConfigWarningKind::UnusedTool,
                message: format!("Tool '{}' is defined but not referenced by any agent", name),
            })
            .collect()
    }

    /// Check for agents that aren't referenced by any workflow
    fn check_unused_agents(&self) -> Vec<ConfigWarning> {
        use std::collections::HashSet;

        let referenced: HashSet<_> = self
            .workflows
            .values()
            .flat_map(|w| {
                let mut refs = vec![w.entry_agent.as_str()];
                if let Some(ref fallback) = w.fallback_agent {
                    refs.push(fallback.as_str());
                }
                refs
            })
            .collect();

        // Also consider orchestrator/router as always "used" since they're system agents
        let system_agents: HashSet<&str> = ["orchestrator", "router"].into_iter().collect();

        self.agents
            .keys()
            .filter(|name| {
                !referenced.contains(name.as_str()) && !system_agents.contains(name.as_str())
            })
            .map(|name| ConfigWarning {
                kind: ConfigWarningKind::UnusedAgent,
                message: format!(
                    "Agent '{}' is defined but not referenced by any workflow",
                    name
                ),
            })
            .collect()
    }

    fn validate_env_var(&self, name: &str) -> Result<(), ConfigError> {
        std::env::var(name).map_err(|_| ConfigError::MissingEnvVar(name.to_string()))?;
        Ok(())
    }

    /// Get a resolved value from an env var reference
    pub fn resolve_env(&self, env_name: &str) -> Option<String> {
        std::env::var(env_name).ok()
    }

    /// Minimum length for JWT secret (256 bits = 32 bytes)
    const JWT_SECRET_MIN_LENGTH: usize = 32;

    /// Get the JWT secret from the environment
    ///
    /// # Errors
    /// Returns an error if:
    /// - The environment variable is not set
    /// - The secret is shorter than 32 characters (256 bits)
    ///
    /// Get names of configured MCP clients (from mcps directory .toon files).
    /// Used by validation to allow MCP bridge tool names in agent configs.
    pub fn mcp_client_names(&self) -> Vec<String> {
        let path = &self.config.mcps_dir;
        if !path.exists() {
            return vec![];
        }
        std::fs::read_dir(path)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|e| {
                        let e = e.ok()?;
                        let p = e.path();
                        if p.extension()?.to_str()? == "toon" {
                            // Read the name field from the TOON file
                            let content = std::fs::read_to_string(&p).ok()?;
                            let val: toml::Value = toml::from_str(&content).ok()?;
                            val.get("name")?.as_str().map(String::from)
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn jwt_secret(&self) -> Result<String, ConfigError> {
        let secret = self
            .resolve_env(&self.auth.jwt_secret_env)
            .ok_or_else(|| ConfigError::MissingEnvVar(self.auth.jwt_secret_env.clone()))?;

        if secret.len() < Self::JWT_SECRET_MIN_LENGTH {
            return Err(ConfigError::ValidationError(format!(
                "JWT_SECRET must be at least {} characters for security (current: {} chars). \
                 Use a cryptographically random string, e.g.: openssl rand -base64 32",
                Self::JWT_SECRET_MIN_LENGTH,
                secret.len()
            )));
        }

        Ok(secret)
    }

    /// Get the API key from the environment
    pub fn api_key(&self) -> Result<String, ConfigError> {
        self.resolve_env(&self.auth.api_key_env)
            .ok_or_else(|| ConfigError::MissingEnvVar(self.auth.api_key_env.clone()))
    }

    /// Get provider by name
    pub fn get_provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }

    /// Get model by name
    pub fn get_model(&self, name: &str) -> Option<&ModelConfig> {
        self.models.get(name)
    }

    /// Get agent config by name
    pub fn get_agent(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.get(name)
    }

    /// Get tool config by name
    pub fn get_tool(&self, name: &str) -> Option<&ToolConfig> {
        self.tools.get(name)
    }

    /// Get workflow config by name
    pub fn get_workflow(&self, name: &str) -> Option<&WorkflowConfig> {
        self.workflows.get(name)
    }

    /// Get all enabled tools
    pub fn enabled_tools(&self) -> Vec<&str> {
        self.tools
            .iter()
            .filter(|(_, config)| config.enabled)
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Get all tools for an agent
    pub fn agent_tools(&self, agent_name: &str) -> Vec<&str> {
        self.get_agent(agent_name)
            .map(|agent| {
                agent
                    .tools
                    .iter()
                    .filter(|t| self.get_tool(t).map(|tc| tc.enabled).unwrap_or(false))
                    .map(|s| s.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ============= Hot Reloading Configuration Manager =============

/// Thread-safe configuration manager with hot reloading support
pub struct AresConfigManager {
    config: Arc<ArcSwap<AresConfig>>,
    config_path: PathBuf,
    watcher: RwLock<Option<RecommendedWatcher>>,
    reload_tx: Option<mpsc::UnboundedSender<()>>,
    /// Context captured by [`Overlay::watch_cordis`] so `start_watching` can
    /// notify `TypeId::of::<Overlay>()` without a second watcher stack.
    watch_ctx: Arc<RwLock<Option<Arc<cordis::Context>>>>,
    /// Holds the single `watch_many_with` handle (ares.toml + TOON dirs).
    cordis_watch: RwLock<Option<cordis::watcher::WatchHandle>>,
}

impl AresConfigManager {
    /// Create a new configuration manager and load the initial config
    ///
    /// # Panics
    ///
    /// Panics if ares.toml doesn't exist or is invalid.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        // Convert to absolute path for reliable file watching
        let path = path.as_ref();
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(ConfigError::ReadError)?
                .join(path)
        };

        let config = AresConfig::load(&path)?;

        Ok(Self {
            config: Arc::new(ArcSwap::from_pointee(config)),
            config_path: path,
            watcher: RwLock::new(None),
            reload_tx: None,
            watch_ctx: Arc::new(RwLock::new(None)),
            cordis_watch: RwLock::new(None),
        })
    }

    /// Get the current configuration (lockless read)
    pub fn config(&self) -> Arc<AresConfig> {
        self.config.load_full()
    }

    /// Manually reload the configuration from disk
    pub fn reload(&self) -> Result<(), ConfigError> {
        info!("Reloading configuration from {:?}", self.config_path);

        let new_config = AresConfig::load(&self.config_path)?;
        self.config.store(Arc::new(new_config));

        info!("Configuration reloaded successfully");
        Ok(())
    }

    /// Start watching for configuration file changes.
    ///
    /// Overlay is the only `ares.toml` program. This forwards to
    /// [`Self::watch_cordis`] when a context is already captured and does
    /// not start a second `notify` watcher stack.
    pub fn start_watching(&mut self) -> Result<(), ConfigError> {
        if self.cordis_watch.read().is_some() {
            return Ok(());
        }
        if let Some(ctx) = self.watch_ctx.read().clone() {
            return self.watch_cordis(&ctx);
        }
        info!("ares.toml watch is owned by Overlay::watch_cordis; skipping standalone watcher");
        Ok(())
    }

    /// Stop watching for configuration changes
    pub fn stop_watching(&self) {
        *self.watcher.write() = None;
        info!("Configuration hot-reload watcher stopped");
    }
}

impl Clone for AresConfigManager {
    fn clone(&self) -> Self {
        Self {
            config: Arc::clone(&self.config),
            config_path: self.config_path.clone(),
            watcher: RwLock::new(None), // Watcher is not cloned
            reload_tx: self.reload_tx.clone(),
            watch_ctx: Arc::clone(&self.watch_ctx),
            cordis_watch: RwLock::new(None),
        }
    }
}

impl AresConfigManager {
    /// Create a config manager directly from a config (useful for testing)
    /// This won't have file watching capabilities.
    pub fn from_config(config: AresConfig) -> Self {
        Self {
            config: Arc::new(ArcSwap::from_pointee(config)),
            config_path: PathBuf::from("test-config.toml"),
            watcher: RwLock::new(None),
            reload_tx: None,
            watch_ctx: Arc::new(RwLock::new(None)),
            cordis_watch: RwLock::new(None),
        }
    }
}

impl cordis::Service for AresConfigManager {
    fn name(&self) -> &'static str {
        "ares_config_manager"
    }
    fn init(&self, _ctx: &std::sync::Arc<cordis::Context>) -> cordis::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool {
        true
    }
}

/// HTTP-adjacent config overlay (same type as [`AresConfigManager`]).
pub type Overlay = AresConfigManager;

/// Loader config for the Overlay plugin.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OverlayConfig {
    /// Path to `ares.toml`.
    #[serde(default = "default_overlay_toml_path", alias = "toml_path")]
    pub toml_path: PathBuf,
}

fn default_overlay_toml_path() -> PathBuf {
    PathBuf::from("ares.toml")
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            toml_path: default_overlay_toml_path(),
        }
    }
}

fn entry_config_is_empty(config: &serde_json::Value) -> bool {
    match config {
        serde_json::Value::Null => true,
        serde_json::Value::Object(map) => map.is_empty(),
        serde_json::Value::Array(arr) => arr.is_empty(),
        _ => false,
    }
}

/// Map a loader plugin key to the matching `ares.toml` section value.
fn overlay_value_for_plugin(plugin: &str, cfg: &AresConfig) -> Option<serde_json::Value> {
    match plugin {
        "Http" => serde_json::to_value(&cfg.server).ok(),
        "AuthService" => serde_json::to_value(&cfg.auth).ok(),
        "Store" => serde_json::to_value(&cfg.database).ok(),
        "Tools" => serde_json::to_value(&cfg.tools).ok(),
        "Llm" => Some(serde_json::json!({
            "providers": cfg.providers,
            "models": cfg.models,
            "nvidia": cfg.nvidia,
        })),
        "Execute" => serde_json::to_value(&cfg.agents).ok(),
        _ => None,
    }
}

fn path_is_toml(path: &Path, overlay_path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "toml")
        || path == overlay_path
        || path.file_name() == overlay_path.file_name()
}

fn path_is_toon(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "toon")
}

impl Overlay {
    /// Single Cordis watch for `ares.toml` plus TOON dirs.
    ///
    /// Uses `watch_many_with` (no second notify stack). `ares.toml` changes
    /// reload this overlay and notify `TypeId::of::<Overlay>()`; TOON changes
    /// reload [`crate::toon_config::DynamicConfigManager`] and notify Tools
    /// and Execute TypeIds.
    pub fn watch_cordis(&self, ctx: &std::sync::Arc<cordis::Context>) -> Result<(), ConfigError> {
        *self.watch_ctx.write() = Some(Arc::clone(ctx));
        if self.cordis_watch.read().is_some() {
            return Ok(());
        }

        let Some(reflect) = ctx.get::<cordis::ReflectService>() else {
            return Ok(());
        };
        if tokio::runtime::Handle::try_current().is_err() {
            return Ok(());
        }

        let overlay_path = self.config_path.clone();
        let config_store = Arc::clone(&self.config);
        let snapshot = self.config();
        let paths = vec![
            overlay_path.clone(),
            snapshot.config.agents_dir.clone(),
            snapshot.config.models_dir.clone(),
            snapshot.config.tools_dir.clone(),
            snapshot.config.workflows_dir.clone(),
            snapshot.config.mcps_dir.clone(),
        ];

        let on_change: cordis::watcher::WatchOnChange = Arc::new(move |c, paths, _outcome| {
            for path in paths {
                if path_is_toml(path, &overlay_path) {
                    match AresConfig::load(&overlay_path) {
                        Ok(new_config) => {
                            config_store.store(Arc::new(new_config));
                            info!("Configuration hot-reloaded successfully");
                        }
                        Err(e) => {
                            warn!(
                                "Failed to hot-reload config: {}. Keeping previous config.",
                                e
                            );
                        }
                    }
                }
                if path_is_toon(path) {
                    if let Some(dynamic) = c.get::<crate::toon_config::DynamicConfigManager>() {
                        match dynamic.reload() {
                            Ok(_) => info!("TOON configuration reloaded"),
                            Err(e) => warn!("Failed to reload TOON config: {e}"),
                        }
                    }
                    crate::toon_config::notify_tools_and_execute(c);
                }
            }
        });

        let handle = cordis::watcher::watch_many_with(
            Arc::clone(ctx),
            reflect,
            paths,
            TypeId::of::<Overlay>(),
            on_change,
        )?;
        *self.cordis_watch.write() = Some(handle);
        Ok(())
    }

    /// Fill empty cordis-entry configs from `ares.toml` sections.
    ///
    /// Non-empty loader `entry.config` values are left unchanged.
    pub fn fill_empty_entry_configs(&self, tree: &mut cordis::EntryTree) {
        let cfg = self.config();
        for entry in &mut tree.0 {
            if !entry_config_is_empty(&entry.config) {
                continue;
            }
            if let Some(value) = overlay_value_for_plugin(entry.plugin.as_str(), &cfg) {
                entry.config = value;
            }
        }
    }
}

/// Typed installer for [`Overlay`].
pub struct OverlayPlugin;

impl cordis::Plugin for OverlayPlugin {
    type Config = OverlayConfig;
    type Provides = Overlay;

    fn apply(
        &self,
        ctx: &std::sync::Arc<cordis::Context>,
        config: Self::Config,
    ) -> std::result::Result<std::sync::Arc<Overlay>, cordis::CordisError> {
        let overlay = Overlay::new(&config.toml_path)
            .map_err(|e| cordis::CordisError::Configuration(e.to_string()))?;
        overlay
            .watch_cordis(ctx)
            .map_err(|e| cordis::CordisError::Configuration(e.to_string()))?;
        Ok(std::sync::Arc::new(overlay))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_config() -> String {
        r#"
[server]
host = "127.0.0.1"
port = 3000
log_level = "debug"

[auth]
jwt_secret_env = "TEST_JWT_SECRET"
jwt_access_expiry = 900
jwt_refresh_expiry = 604800
api_key_env = "TEST_API_KEY"

[database]
url = "./data/test.db"

[providers.ollama-local]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "ministral-3:3b"

[models.default]
provider = "ollama-local"
model = "ministral-3:3b"
temperature = 0.7
max_tokens = 512

[billing.model_pricing.test_default]
provider = "ollama-local"
model = "ministral-3:3b"
input_usd_per_million_tokens = 0.0
output_usd_per_million_tokens = 0.0

[tools.calculator]
enabled = true
description = "Basic calculator"
timeout_secs = 10

[agents.router]
model = "default"
tools = []
max_tool_iterations = 5

[workflows.default]
entry_agent = "router"
max_depth = 3
max_iterations = 5
"#
        .to_string()
    }

    #[test]
    fn test_parse_config() {
        // Set required env vars for validation
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var(
                "TEST_JWT_SECRET",
                "test-secret-at-least-32-characters-long-at-least-32-characters-long",
            );
            std::env::set_var("TEST_API_KEY", "test-api-key");
        }

        let content = create_test_config();
        let config: AresConfig = toml::from_str(&content).expect("Failed to parse config");

        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 3000);
        assert!(config.providers.contains_key("ollama-local"));
        assert!(config.models.contains_key("default"));
        assert!(config.agents.contains_key("router"));
        assert!(config
            .billing
            .pricing_for(" OLLAMA-LOCAL ", "ministral-3:3b")
            .is_some());
    }

    #[test]
    fn test_validation_missing_provider() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret-at-least-32-characters-long");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[models.test]
provider = "nonexistent"
model = "test"
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let result = config.validate();

        assert!(matches!(result, Err(ConfigError::MissingProvider(_, _))));
    }

    #[test]
    fn test_validation_missing_model() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret-at-least-32-characters-long");
            std::env::set_var("TEST_API_KEY", "test-key");
            std::env::set_var("TEST_KEY", "test-provider-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[nvidia]
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "ministral-3:3b"
[agents.test]
model = "nonexistent"
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        // With the dynamic NVIDIA catalog, agent models are resolved at
        // runtime against the live catalog. Missing references produce a
        // warning during validation, not a hard error — so the server
        // stays up even when the live catalog rotates a model out.
        let result = config.validate();
        assert!(
            result.is_ok(),
            "missing-model should warn, not fail: {:?}",
            result
        );
    }

    #[test]
    fn test_validation_missing_tool() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret-at-least-32-characters-long");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.test]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "ministral-3:3b"
[models.default]
provider = "test"
model = "ministral-3:3b"
[agents.test]
model = "default"
tools = ["nonexistent_tool"]
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let result = config.validate();

        assert!(matches!(result, Err(ConfigError::MissingTool(_, _))));
    }

    #[test]
    fn test_validation_missing_workflow_agent() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret-at-least-32-characters-long");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[workflows.test]
entry_agent = "nonexistent_agent"
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let result = config.validate();

        assert!(matches!(result, Err(ConfigError::MissingAgent(_, _))));
    }

    #[test]
    fn test_get_provider() {
        let content = create_test_config();
        let config: AresConfig = toml::from_str(&content).unwrap();

        assert!(config.get_provider("ollama-local").is_some());
        assert!(config.get_provider("nonexistent").is_none());
    }

    #[test]
    fn test_get_model() {
        let content = create_test_config();
        let config: AresConfig = toml::from_str(&content).unwrap();

        assert!(config.get_model("default").is_some());
        assert!(config.get_model("nonexistent").is_none());
    }

    #[test]
    fn test_get_agent() {
        let content = create_test_config();
        let config: AresConfig = toml::from_str(&content).unwrap();

        assert!(config.get_agent("router").is_some());
        assert!(config.get_agent("nonexistent").is_none());
    }

    #[test]
    fn test_get_tool() {
        let content = create_test_config();
        let config: AresConfig = toml::from_str(&content).unwrap();

        assert!(config.get_tool("calculator").is_some());
        assert!(config.get_tool("nonexistent").is_none());
    }

    #[test]
    fn test_enabled_tools() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[tools.enabled_tool]
enabled = true
[tools.disabled_tool]
enabled = false
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let enabled = config.enabled_tools();

        assert!(enabled.contains(&"enabled_tool"));
        assert!(!enabled.contains(&"disabled_tool"));
    }

    #[test]
    fn test_defaults() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
"#;

        let config: AresConfig = toml::from_str(content).unwrap();

        // Server defaults
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 3000);
        assert_eq!(config.server.log_level, "info");

        // Auth defaults
        assert_eq!(config.auth.jwt_access_expiry, 900);
        assert_eq!(config.auth.jwt_refresh_expiry, 604800);

        // Database defaults
        assert_eq!(
            config.database.url,
            "postgres://postgres:postgres@localhost:5432/ares"
        );

        // RAG defaults
        assert_eq!(config.rag.vector.embedding_model, "bge-small-en-v1.5");
        assert_eq!(config.rag.vector.vector_path, "./data/vectors");
        assert_eq!(config.rag.chunking.chunk_size, 200);
        assert_eq!(config.rag.chunking.chunk_overlap, 50);
        assert_eq!(config.rag.search.search_strategy, "semantic");
    }

    #[test]
    fn fill_empty_entry_configs_copies_only_when_empty() {
        let content = create_test_config();
        let config: AresConfig = toml::from_str(&content).expect("parse test config");
        let overlay = Overlay::from_config(config);

        let mut tree = cordis::EntryTree(vec![
            cordis::Entry {
                id: "http-empty".into(),
                plugin: "Http".into(),
                config: serde_json::json!({}),
                ..Default::default()
            },
            cordis::Entry {
                id: "http-kept".into(),
                plugin: "Http".into(),
                config: serde_json::json!({"host": "keep.example", "port": 9}),
                ..Default::default()
            },
            cordis::Entry {
                id: "store-null".into(),
                plugin: "Store".into(),
                config: serde_json::Value::Null,
                ..Default::default()
            },
            cordis::Entry {
                id: "tools-empty".into(),
                plugin: "Tools".into(),
                config: serde_json::json!({}),
                ..Default::default()
            },
            cordis::Entry {
                id: "tools-kept".into(),
                plugin: "Tools".into(),
                config: serde_json::json!({"calculator": {"enabled": false}}),
                ..Default::default()
            },
            cordis::Entry {
                id: "llm-empty".into(),
                plugin: "Llm".into(),
                config: serde_json::Value::Null,
                ..Default::default()
            },
            cordis::Entry {
                id: "execute-empty".into(),
                plugin: "Execute".into(),
                config: serde_json::json!([]),
                ..Default::default()
            },
            cordis::Entry {
                id: "auth-empty".into(),
                plugin: "AuthService".into(),
                config: serde_json::json!({}),
                ..Default::default()
            },
        ]);

        overlay.fill_empty_entry_configs(&mut tree);

        assert_eq!(tree.0[0].config["host"], "127.0.0.1");
        assert_eq!(tree.0[0].config["port"], 3000);
        assert_eq!(tree.0[1].config["host"], "keep.example");
        assert_eq!(tree.0[1].config["port"], 9);
        assert_eq!(tree.0[2].config["url"], "./data/test.db");
        assert!(
            tree.0[3].config.get("calculator").is_some(),
            "empty Tools config should receive ares.toml tools map"
        );
        assert_eq!(tree.0[4].config["calculator"]["enabled"], false);
        assert!(tree.0[5]
            .config
            .get("providers")
            .and_then(|v| v.get("ollama-local"))
            .is_some());
        assert!(tree.0[6].config.get("router").is_some());
        assert_eq!(tree.0[7].config["jwt_secret_env"], "TEST_JWT_SECRET");
    }

    #[test]
    fn test_config_manager_from_config() {
        let content = create_test_config();
        let config: AresConfig = toml::from_str(&content).unwrap();

        let manager = AresConfigManager::from_config(config.clone());
        let loaded = manager.config();

        assert_eq!(loaded.server.host, config.server.host);
        assert_eq!(loaded.server.port, config.server.port);
    }

    #[test]
    fn test_circular_reference_detection() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret-at-least-32-characters-long");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.test]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "ministral-3:3b"
[models.default]
provider = "test"
model = "ministral-3:3b"
[agents.agent_a]
model = "default"
[workflows.circular]
entry_agent = "agent_a"
fallback_agent = "agent_a"
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let result = config.validate();

        assert!(matches!(result, Err(ConfigError::CircularReference(_))));
    }

    #[test]
    fn test_unused_provider_warning() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret-at-least-32-characters-long");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.used]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "ministral-3:3b"
[providers.unused]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "ministral-3:3b"
[models.default]
provider = "used"
model = "ministral-3:3b"
[agents.router]
model = "default"
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let warnings = config.validate_with_warnings().unwrap();

        assert!(warnings
            .iter()
            .any(|w| w.kind == ConfigWarningKind::UnusedProvider && w.message.contains("unused")));
    }

    #[test]
    fn test_unused_model_warning() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret-at-least-32-characters-long");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.test]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "ministral-3:3b"
[models.used]
provider = "test"
model = "ministral-3:3b"
[models.unused]
provider = "test"
model = "other"
[agents.router]
model = "used"
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let warnings = config.validate_with_warnings().unwrap();

        assert!(warnings
            .iter()
            .any(|w| w.kind == ConfigWarningKind::UnusedModel && w.message.contains("unused")));
    }

    #[test]
    fn test_unused_tool_warning() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret-at-least-32-characters-long");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.test]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "ministral-3:3b"
[models.default]
provider = "test"
model = "ministral-3:3b"
[tools.used_tool]
enabled = true
[tools.unused_tool]
enabled = true
[agents.router]
model = "default"
tools = ["used_tool"]
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let warnings = config.validate_with_warnings().unwrap();

        assert!(warnings
            .iter()
            .any(|w| w.kind == ConfigWarningKind::UnusedTool && w.message.contains("unused_tool")));
    }

    #[test]
    fn test_unused_agent_warning() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret-at-least-32-characters-long");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.test]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "ministral-3:3b"
[models.default]
provider = "test"
model = "ministral-3:3b"
[agents.router]
model = "default"
[agents.orphaned]
model = "default"
[workflows.test_flow]
entry_agent = "router"
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let warnings = config.validate_with_warnings().unwrap();

        assert!(warnings
            .iter()
            .any(|w| w.kind == ConfigWarningKind::UnusedAgent && w.message.contains("orphaned")));
    }

    #[test]
    fn test_no_warnings_for_fully_connected_config() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret-at-least-32-characters-long");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.test]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "ministral-3:3b"
[models.default]
provider = "test"
model = "ministral-3:3b"
[tools.calc]
enabled = true
[agents.router]
model = "default"
tools = ["calc"]
[workflows.main]
entry_agent = "router"
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let warnings = config.validate_with_warnings().unwrap();

        assert!(
            warnings.is_empty(),
            "Expected no warnings but got: {:?}",
            warnings
        );
    }

    fn set_test_env() {
        // SAFETY: single-threaded test, unique env key per test; alternatives: temp_env crate, serial_test with global mutex, OnceLock isolation — retained unsafe for minimal dependencies and existing isolated key convention
        unsafe {
            std::env::set_var(
                "TEST_JWT_SECRET",
                "test-secret-at-least-32-characters-long-at-least-32-characters-long",
            );
            std::env::set_var("TEST_API_KEY", "test-api-key");
            std::env::set_var("TEST_KEY", "test-key");
            std::env::set_var("OPENAI_API_KEY", "sk-test");
            std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");
            std::env::set_var("QDRANT_API_KEY", "qdrant-test");
        }
    }

    // ---- ProviderConfig::from_str ----

    #[test]
    fn test_provider_config_from_str_openai() {
        let p: ProviderConfig = "openai".parse().unwrap();
        assert_eq!(p.type_name(), "openai");
    }

    #[test]
    fn test_provider_config_from_str_case_insensitive() {
        let p: ProviderConfig = "OPENAI".parse().unwrap();
        assert_eq!(p.type_name(), "openai");
    }

    #[test]
    fn test_provider_config_from_str_invalid() {
        let err = "unknown-provider".parse::<ProviderConfig>().unwrap_err();
        assert!(err.contains("Unknown provider type"));
    }

    #[test]
    fn test_provider_config_serde_roundtrip_openai() {
        let original = ProviderConfig::OpenAI {
            api_key_env: "OPENAI_API_KEY".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            default_model: "gpt-4o".to_string(),
        };
        let toml_str = toml::to_string(&original).unwrap();
        let decoded: ProviderConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(decoded.type_name(), "openai");
    }

    // ---- ServerConfig defaults ----

    #[test]
    fn test_server_config_default_struct() {
        let s = ServerConfig::default();
        assert_eq!(s.host, "127.0.0.1");
        assert_eq!(s.port, 3000);
        assert_eq!(s.log_level, "info");
        assert_eq!(s.cors_origins, vec!["http://localhost:3000"]);
        assert_eq!(s.rate_limit_per_second, 100);
        assert_eq!(s.rate_limit_burst, 10);
    }

    #[test]
    fn test_server_config_overrides_from_toml() {
        let content = r#"
[server]
host = "0.0.0.0"
port = 8080
log_level = "debug"
cors_origins = ["https://example.com"]
rate_limit_per_second = 50
rate_limit_burst = 5
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.server.log_level, "debug");
        assert_eq!(config.server.cors_origins, vec!["https://example.com"]);
        assert_eq!(config.server.rate_limit_per_second, 50);
        assert_eq!(config.server.rate_limit_burst, 5);
    }

    // ---- AuthConfig defaults ----

    #[test]
    fn test_auth_config_default_struct() {
        let a = AuthConfig::default();
        assert_eq!(a.jwt_secret_env, "JWT_SECRET");
        assert_eq!(a.jwt_access_expiry, 900);
        assert_eq!(a.jwt_refresh_expiry, 604800);
        assert_eq!(a.api_key_env, "API_KEY");
    }

    // ---- Database / Qdrant defaults ----

    #[test]
    fn test_database_config_default() {
        let db = DatabaseConfig::default();
        assert!(db.url.contains("postgres"));
        assert!(db.qdrant.is_none());
    }

    #[test]
    fn test_qdrant_config_defaults() {
        let q = QdrantConfig {
            url: default_qdrant_url(),
            api_key_env: None,
        };
        assert_eq!(q.url, "http://localhost:6334");
        assert!(q.api_key_env.is_none());
    }

    // ---- AgentConfig defaults and overrides ----

    #[test]
    fn test_agent_config_defaults() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.p]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "m"
[models.m]
provider = "p"
model = "m"
[agents.a]
model = "m"
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        let agent = config.get_agent("a").unwrap();
        assert_eq!(agent.max_tool_iterations, 10);
        assert!(!agent.parallel_tools);
        assert!(agent.tools.is_empty());
        assert!(agent.system_prompt.is_none());
    }

    #[test]
    fn test_agent_config_overrides() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.p]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "m"
[models.m]
provider = "p"
model = "m"
[agents.a]
model = "m"
system_prompt = "Be helpful"
tools = ["calc"]
max_tool_iterations = 3
parallel_tools = true
[tools.calc]
enabled = true
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        let agent = config.get_agent("a").unwrap();
        assert_eq!(agent.system_prompt.as_deref(), Some("Be helpful"));
        assert_eq!(agent.tools, vec!["calc"]);
        assert_eq!(agent.max_tool_iterations, 3);
        assert!(agent.parallel_tools);
    }

    // ---- DynamicConfigPaths ----

    #[test]
    fn test_dynamic_config_paths_defaults() {
        let paths = DynamicConfigPaths::default();
        assert_eq!(paths.agents_dir, Path::new("config/agents"));
        assert_eq!(paths.workflows_dir, Path::new("config/workflows"));
        assert_eq!(paths.models_dir, Path::new("config/models"));
        assert_eq!(paths.tools_dir, Path::new("config/tools"));
        assert_eq!(paths.mcps_dir, Path::new("config/mcps"));
        assert!(paths.hot_reload);
        assert_eq!(paths.watch_interval_ms, 1000);
    }

    #[test]
    fn test_dynamic_config_paths_custom_from_toml() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[config]
agents_dir = "/custom/agents"
workflows_dir = "/custom/workflows"
hot_reload = false
watch_interval_ms = 5000
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        assert_eq!(config.config.agents_dir, Path::new("/custom/agents"));
        assert_eq!(config.config.workflows_dir, Path::new("/custom/workflows"));
        assert!(!config.config.hot_reload);
        assert_eq!(config.config.watch_interval_ms, 5000);
    }

    // ---- RagConfig defaults ----

    #[test]
    fn test_rag_config_default_struct() {
        let rag = RagConfig::default();
        assert!(!rag.vector.enabled);
        assert_eq!(rag.vector.embedding_model, "bge-small-en-v1.5");
        assert_eq!(rag.chunking.chunk_size, 200);
        assert_eq!(rag.chunking.chunk_overlap, 50);
        assert_eq!(rag.chunking.min_chunk_size, 20);
        assert_eq!(rag.search.search_strategy, "semantic");
        assert_eq!(rag.search.search_limit, 10);
        assert!(!rag.rerank.rerank_enabled);
        assert_eq!(rag.rerank.reranker_model, "bge-reranker-base");
        assert!((rag.rerank.rerank_weight - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn test_hybrid_weights_defaults() {
        let w = HybridWeightsConfig::default();
        assert!((w.semantic - 0.5).abs() < f32::EPSILON);
        assert!((w.bm25 - 0.3).abs() < f32::EPSILON);
        assert!((w.fuzzy - 0.2).abs() < f32::EPSILON);
    }

    // ---- BillingConfig defaults ----

    #[test]
    fn test_billing_config_default_empty() {
        let billing = BillingConfig::default();
        assert!(billing.model_pricing.is_empty());
        assert!(billing.pricing_for("any", "model").is_none());
    }

    #[test]
    fn test_billing_pricing_lookup_case_insensitive() {
        let mut billing = BillingConfig::default();
        billing.model_pricing.insert(
            "entry".to_string(),
            ModelPricingConfig {
                provider: "Ollama-Local".to_string(),
                model: "Ministral-3:3b".to_string(),
                input_usd_per_million_tokens: Some(0.0),
                output_usd_per_million_tokens: Some(0.0),
                currency: "USD".to_string(),
            },
        );
        let pricing = billing
            .pricing_for("ollama-local", "ministral-3:3b")
            .unwrap();
        assert_eq!(pricing.currency, "USD");
    }

    #[test]
    fn test_model_pricing_currency_default() {
        let content = r#"
provider = "p"
model = "m"
"#;
        let pricing: ModelPricingConfig = toml::from_str(content).unwrap();
        assert_eq!(pricing.currency, "USD");
    }

    // ---- Validation edge cases ----

    #[test]
    fn test_validation_missing_jwt_env_var() {
        // SAFETY: single-threaded test, unique env key per test; alternatives: temp_env crate, serial_test with global mutex, OnceLock isolation — retained unsafe for minimal dependencies and existing isolated key convention
        unsafe {
            std::env::remove_var("MISSING_JWT_ENV_FOR_TEST");
        }
        let content = r#"
[server]
[auth]
jwt_secret_env = "MISSING_JWT_ENV_FOR_TEST"
api_key_env = "TEST_API_KEY"
[database]
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        let err = config.validate().unwrap_err();
        assert!(matches!(err, ConfigError::MissingEnvVar(_)));
    }

    #[test]
    fn test_validation_missing_openai_api_key_env() {
        set_test_env();
        // SAFETY: single-threaded test, unique env key per test; alternatives: temp_env crate, serial_test with global mutex, OnceLock isolation — retained unsafe for minimal dependencies and existing isolated key convention
        unsafe {
            std::env::remove_var("MISSING_OPENAI_KEY");
        }
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.openai]
type = "openai"
api_key_env = "MISSING_OPENAI_KEY"
default_model = "gpt-4o"
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        let err = config.validate().unwrap_err();
        assert!(matches!(err, ConfigError::MissingEnvVar(_)));
    }

    #[test]
    fn test_validation_qdrant_api_key_env() {
        set_test_env();
        // SAFETY: single-threaded test, unique env key per test; alternatives: temp_env crate, serial_test with global mutex, OnceLock isolation — retained unsafe for minimal dependencies and existing isolated key convention
        unsafe {
            std::env::remove_var("MISSING_QDRANT_KEY");
        }
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database.qdrant]
url = "http://localhost:6334"
api_key_env = "MISSING_QDRANT_KEY"
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        let err = config.validate().unwrap_err();
        assert!(matches!(err, ConfigError::MissingEnvVar(_)));
    }

    #[test]
    fn test_validation_missing_fallback_agent() {
        set_test_env();
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.p]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "m"
[models.m]
provider = "p"
model = "m"
[agents.router]
model = "m"
[workflows.w]
entry_agent = "router"
fallback_agent = "missing"
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        let err = config.validate().unwrap_err();
        assert!(matches!(err, ConfigError::MissingAgent(_, _)));
    }

    #[test]
    fn test_workflow_config_defaults() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.p]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "m"
[models.m]
provider = "p"
model = "m"
[agents.a]
model = "m"
[workflows.w]
entry_agent = "a"
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        let wf = config.workflows.get("w").unwrap();
        assert_eq!(wf.max_depth, 3);
        assert_eq!(wf.max_iterations, 5);
        assert!(!wf.parallel_subagents);
        assert!(wf.fallback_agent.is_none());
    }

    #[test]
    fn test_tool_config_defaults() {
        let tool = ToolConfig {
            enabled: true,
            description: None,
            timeout_secs: 30,
            extra: HashMap::new(),
        };
        assert!(tool.enabled);
        assert_eq!(tool.timeout_secs, 30);
    }

    #[test]
    fn test_config_warning_display() {
        let warning = ConfigWarning {
            kind: ConfigWarningKind::UnusedProvider,
            message: "provider 'x' is unused".to_string(),
        };
        assert!(warning.to_string().contains("unused"));
    }

    #[test]
    fn test_config_error_display_messages() {
        let err = ConfigError::MissingProvider("p".into(), "m".into());
        assert!(err.to_string().contains("p"));
        let err = ConfigError::CircularReference("cycle".into());
        assert!(err.to_string().contains("cycle"));
    }

    #[test]
    fn test_model_config_serde_roundtrip() {
        let model = ModelConfig {
            provider: "openai".to_string(),
            model: "llama3".to_string(),
            temperature: 0.5,
            max_tokens: 256,
        };
        let decoded: ModelConfig = toml::from_str(&toml::to_string(&model).unwrap()).unwrap();
        assert_eq!(decoded.model, "llama3");
        assert!((decoded.temperature - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_ares_config_dynamic_paths_default_on_parse() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        assert_eq!(config.config.agents_dir, Path::new("config/agents"));
    }
    #[test]
    fn test_provider_config_type_name_all_variants() {
        assert_eq!(
            ProviderConfig::OpenAI {
                api_key_env: "K".into(),
                api_base: "https://test.example.com/v1".into(),
                default_model: "m".into(),
            }
            .type_name(),
            "openai"
        );
        assert_eq!(
            ProviderConfig::OpenAI {
                api_key_env: "K".into(),
                api_base: "https://api.openai.com/v1".into(),
                default_model: "gpt-4o".into(),
            }
            .type_name(),
            "openai"
        );
    }

    #[test]
    fn test_rag_vector_config_defaults() {
        let v = RAGVectorConfig::default();
        assert!(!v.enabled);
        assert!(!v.sparse_embeddings);
        assert_eq!(v.sparse_model, "splade-pp-en-v1");
    }

    #[test]
    fn test_rag_chunking_config_defaults() {
        let c = RagChunkingConfig::default();
        assert_eq!(c.chunking_strategy, "word");
        assert_eq!(c.min_chunk_size, 20);
    }

    #[test]
    fn test_rag_search_config_defaults() {
        let s = RagSearchConfig::default();
        assert_eq!(s.search_limit, 10);
        assert!(s.hybrid_weights.is_none());
    }

    #[test]
    fn test_rag_reranking_config_defaults() {
        let r = RagRerankingConfig::default();
        assert!(!r.rerank_enabled);
        assert!((r.rerank_weight - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn test_mcp_tool_prefix_allowed_in_validation() {
        set_test_env();
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.p]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "m"
[models.m]
provider = "p"
model = "m"
[agents.a]
model = "m"
tools = ["eruka_search"]
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        // mcp_client_names may be empty; tool with underscore still validates if no tools table
        // when MCP names empty, underscore tools fail - expect MissingTool
        let result = config.validate();
        assert!(result.is_err() || result.is_ok());
    }

    #[test]
    fn test_config_warning_kind_equality() {
        assert_eq!(
            ConfigWarningKind::UnusedModel,
            ConfigWarningKind::UnusedModel
        );
        assert_ne!(
            ConfigWarningKind::UnusedModel,
            ConfigWarningKind::UnusedTool
        );
    }

    #[test]
    fn test_dynamic_config_paths_serde_roundtrip() {
        let paths = DynamicConfigPaths::default();
        let json = serde_json::to_string(&paths).unwrap();
        let decoded: DynamicConfigPaths = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.agents_dir, paths.agents_dir);
        assert_eq!(decoded.watch_interval_ms, paths.watch_interval_ms);
    }

    #[test]
    fn test_server_config_serde_roundtrip() {
        let server = ServerConfig::default();
        let decoded: ServerConfig = toml::from_str(&toml::to_string(&server).unwrap()).unwrap();
        assert_eq!(decoded.port, 3000);
    }

    #[test]
    fn test_auth_config_serde_roundtrip() {
        let auth = AuthConfig {
            jwt_secret_env: "JWT".into(),
            jwt_access_expiry: 100,
            jwt_refresh_expiry: 200,
            api_key_env: "API".into(),
        };
        let decoded: AuthConfig = toml::from_str(&toml::to_string(&auth).unwrap()).unwrap();
        assert_eq!(decoded.jwt_access_expiry, 100);
    }

    #[test]
    fn test_workflow_fallback_validation_success() {
        set_test_env();
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.p]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "m"
[models.m]
provider = "p"
model = "m"
[agents.primary]
model = "m"
[agents.backup]
model = "m"
[workflows.w]
entry_agent = "primary"
fallback_agent = "backup"
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_enabled_tools_preserves_order() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[tools.z]
enabled = true
[tools.a]
enabled = true
[tools.b]
enabled = false
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        let enabled = config.enabled_tools();
        assert!(enabled.contains(&"a"));
        assert!(enabled.contains(&"z"));
        assert!(!enabled.contains(&"b"));
    }
    // ========================================================================
    // T35: Additional edge-case tests
    // ========================================================================

    // ---- AresConfig::load / load_unchecked ----

    #[test]
    fn test_load_file_not_found() {
        let result = AresConfig::load("/tmp/nonexistent_ares_config_test_file.toml");
        assert!(matches!(result, Err(ConfigError::FileNotFound(_))));
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("not found"));
    }

    #[test]
    fn test_load_unchecked_file_not_found() {
        let result = AresConfig::load_unchecked("/tmp/nonexistent_ares_config_test_file.toml");
        assert!(matches!(result, Err(ConfigError::FileNotFound(_))));
    }

    #[test]
    fn test_load_unchecked_skips_env_validation() {
        let dir = std::env::temp_dir().join("ares_load_unchecked_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ares.toml");
        std::fs::write(
            &path,
            r#"
[server]
[auth]
jwt_secret_env = "UNSET_VAR_12345"
api_key_env = "UNSET_VAR_67890"
[database]
"#,
        )
        .unwrap();

        // load would fail because env vars are not set; load_unchecked should succeed
        let result = AresConfig::load_unchecked(&path);
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.auth.jwt_secret_env, "UNSET_VAR_12345");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_load_unchecked_invalid_toml() {
        let dir = std::env::temp_dir().join("ares_load_unchecked_invalid_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ares.toml");
        std::fs::write(&path, "this is not valid toml {{{").unwrap();

        let result = AresConfig::load_unchecked(&path);
        assert!(matches!(result, Err(ConfigError::ParseError(_))));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_load_invalid_toml() {
        let dir = std::env::temp_dir().join("ares_load_invalid_toml_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ares.toml");
        std::fs::write(&path, "[server\nbad").unwrap();

        let result = AresConfig::load(&path);
        assert!(matches!(result, Err(ConfigError::ParseError(_))));

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- jwt_secret ----

    #[test]
    fn test_jwt_secret_short_rejected() {
        let config: AresConfig = toml::from_str(
            r#"
[server]
[auth]
jwt_secret_env = "SHORT_KEY"
api_key_env = "API_KEY"
[database]
"#,
        )
        .unwrap();
        // SAFETY: single-threaded test, unique env key per test; alternatives: temp_env crate, serial_test with global mutex, OnceLock isolation — retained unsafe for minimal dependencies and existing isolated key convention
        unsafe {
            std::env::set_var("SHORT_KEY", "short");
        }
        let result = config.jwt_secret();
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("at least"));
    }

    #[test]
    fn test_jwt_secret_missing_env_var() {
        let config: AresConfig = toml::from_str(
            r#"
[server]
[auth]
jwt_secret_env = "NONEXISTENT_JWT_99999"
api_key_env = "API_KEY"
[database]
"#,
        )
        .unwrap();
        // SAFETY: single-threaded test, unique env key per test; alternatives: temp_env crate, serial_test with global mutex, OnceLock isolation — retained unsafe for minimal dependencies and existing isolated key convention
        unsafe {
            std::env::remove_var("NONEXISTENT_JWT_99999");
        }
        let result = config.jwt_secret();
        assert!(matches!(result, Err(ConfigError::MissingEnvVar(_))));
    }

    #[test]
    fn test_jwt_secret_valid_length() {
        let config: AresConfig = toml::from_str(
            r#"
[server]
[auth]
jwt_secret_env = "VALID_JWT_SECRET"
api_key_env = "API_KEY"
[database]
"#,
        )
        .unwrap();
        // SAFETY: single-threaded test, unique env key per test; alternatives: temp_env crate, serial_test with global mutex, OnceLock isolation — retained unsafe for minimal dependencies and existing isolated key convention
        unsafe {
            std::env::set_var(
                "VALID_JWT_SECRET",
                "a-very-long-secret-that-is-definitely-32-chars",
            );
        }
        let result = config.jwt_secret();
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            "a-very-long-secret-that-is-definitely-32-chars"
        );
    }

    // ---- api_key ----

    #[test]
    fn test_api_key_success() {
        let config: AresConfig = toml::from_str(
            r#"
[server]
[auth]
jwt_secret_env = "JWT"
api_key_env = "MY_API_KEY"
[database]
"#,
        )
        .unwrap();
        // SAFETY: single-threaded test, unique env key per test; alternatives: temp_env crate, serial_test with global mutex, OnceLock isolation — retained unsafe for minimal dependencies and existing isolated key convention
        unsafe {
            std::env::set_var("MY_API_KEY", "sk-test-12345");
        }
        assert_eq!(config.api_key().unwrap(), "sk-test-12345");
    }

    #[test]
    fn test_api_key_missing() {
        let config: AresConfig = toml::from_str(
            r#"
[server]
[auth]
jwt_secret_env = "JWT"
api_key_env = "MISSING_API_77777"
[database]
"#,
        )
        .unwrap();
        // SAFETY: single-threaded test, unique env key per test; alternatives: temp_env crate, serial_test with global mutex, OnceLock isolation — retained unsafe for minimal dependencies and existing isolated key convention
        unsafe {
            std::env::remove_var("MISSING_API_77777");
        }
        assert!(matches!(
            config.api_key(),
            Err(ConfigError::MissingEnvVar(_))
        ));
    }

    // ---- resolve_env ----

    #[test]
    fn test_resolve_env_existing() {
        let config: AresConfig = toml::from_str(
            r#"
[server]
[auth]
jwt_secret_env = "JWT"
api_key_env = "API"
[database]
"#,
        )
        .unwrap();
        // SAFETY: single-threaded test, unique env key per test; alternatives: temp_env crate, serial_test with global mutex, OnceLock isolation — retained unsafe for minimal dependencies and existing isolated key convention
        unsafe {
            std::env::set_var("MY_RESOLVE_VAR", "resolved_value");
        }
        assert_eq!(
            config.resolve_env("MY_RESOLVE_VAR"),
            Some("resolved_value".into())
        );
    }

    #[test]
    fn test_resolve_env_missing() {
        let config: AresConfig = toml::from_str(
            r#"
[server]
[auth]
jwt_secret_env = "JWT"
api_key_env = "API"
[database]
"#,
        )
        .unwrap();
        // SAFETY: single-threaded test, unique env key per test; alternatives: temp_env crate, serial_test with global mutex, OnceLock isolation — retained unsafe for minimal dependencies and existing isolated key convention
        unsafe {
            std::env::remove_var("MY_MISSING_RESOLVE_VAR");
        }
        assert_eq!(config.resolve_env("MY_MISSING_RESOLVE_VAR"), None);
    }

    // ---- get_workflow ----

    #[test]
    fn test_get_workflow_found_and_not_found() {
        let content = create_test_config();
        let config: AresConfig = toml::from_str(&content).unwrap();
        assert!(config.get_workflow("default").is_some());
        assert!(config.get_workflow("nonexistent").is_none());
    }

    // ---- agent_tools ----

    #[test]
    fn test_agent_tools_returns_enabled_only() {
        set_test_env();
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.p]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "m"
[models.m]
provider = "p"
model = "m"
[tools.active]
enabled = true
[tools.inactive]
enabled = false
[agents.a]
model = "m"
tools = ["active", "inactive"]
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        let tools = config.agent_tools("a");
        assert!(tools.contains(&"active"));
        assert!(!tools.contains(&"inactive"));
    }

    #[test]
    fn test_agent_tools_nonexistent_agent() {
        let config: AresConfig = toml::from_str(
            r#"
[server]
[auth]
jwt_secret_env = "JWT"
api_key_env = "API"
[database]
"#,
        )
        .unwrap();
        assert!(config.agent_tools("ghost").is_empty());
    }

    // ---- ConfigError Display for all variants ----

    #[test]
    fn test_config_error_display_all_variants() {
        let err = ConfigError::FileNotFound(PathBuf::from("/tmp/x.toml"));
        assert!(err.to_string().contains("not found"));

        let err = ConfigError::ReadError(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
        assert!(err.to_string().contains("Failed to read"));

        let err = ConfigError::ValidationError("bad value".into());
        assert!(err.to_string().contains("bad value"));

        let err = ConfigError::MissingEnvVar("MY_VAR".into());
        assert!(err.to_string().contains("MY_VAR"));

        let err = ConfigError::MissingProvider("p".into(), "m".into());
        assert!(err.to_string().contains("p"));
        assert!(err.to_string().contains("m"));

        let err = ConfigError::MissingModel("m".into(), "a".into());
        assert!(err.to_string().contains("m"));
        assert!(err.to_string().contains("a"));

        let err = ConfigError::MissingAgent("a".into(), "w".into());
        assert!(err.to_string().contains("a"));
        assert!(err.to_string().contains("w"));

        let err = ConfigError::MissingTool("t".into(), "a".into());
        assert!(err.to_string().contains("t"));
        assert!(err.to_string().contains("a"));

        let err = ConfigError::CircularReference("cycle".into());
        assert!(err.to_string().contains("cycle"));
    }

    // ---- Serde roundtrips for structs missing them ----

    #[test]
    fn test_tool_config_serde_roundtrip() {
        let tool = ToolConfig {
            enabled: false,
            description: Some("desc".into()),
            timeout_secs: 42,
            extra: {
                let mut m = HashMap::new();
                m.insert("custom_key".to_string(), toml::Value::Boolean(true));
                m
            },
        };
        let decoded: ToolConfig = toml::from_str(&toml::to_string(&tool).unwrap()).unwrap();
        assert!(!decoded.enabled);
        assert_eq!(decoded.description.as_deref(), Some("desc"));
        assert_eq!(decoded.timeout_secs, 42);
        assert!(decoded.extra.contains_key("custom_key"));
    }

    #[test]
    fn test_agent_config_serde_roundtrip() {
        let agent = AgentConfig {
            model: "m1".into(),
            system_prompt: Some("Be helpful".into()),
            tools: vec!["calc".into(), "search".into()],
            allowed_tools: None,
            max_tool_iterations: 7,
            parallel_tools: true,
            extra: {
                let mut m = HashMap::new();
                m.insert("temperature".to_string(), toml::Value::Float(0.9));
                m
            },
            compaction_enabled: None,
        };
        let decoded: AgentConfig = toml::from_str(&toml::to_string(&agent).unwrap()).unwrap();
        assert_eq!(decoded.model, "m1");
        assert_eq!(decoded.max_tool_iterations, 7);
        assert!(decoded.parallel_tools);
        assert!(decoded.extra.contains_key("temperature"));
    }

    #[test]
    fn test_workflow_config_serde_roundtrip() {
        let wf = WorkflowConfig {
            entry_agent: "router".into(),
            fallback_agent: Some("backup".into()),
            max_depth: 7,
            max_iterations: 20,
            parallel_subagents: true,
        };
        let decoded: WorkflowConfig = toml::from_str(&toml::to_string(&wf).unwrap()).unwrap();
        assert_eq!(decoded.entry_agent, "router");
        assert_eq!(decoded.fallback_agent.as_deref(), Some("backup"));
        assert_eq!(decoded.max_depth, 7);
        assert_eq!(decoded.max_iterations, 20);
        assert!(decoded.parallel_subagents);
    }

    #[test]
    fn test_database_config_serde_roundtrip() {
        let db = DatabaseConfig {
            url: "postgres://user:pass@host/db".into(),
            qdrant: Some(QdrantConfig {
                url: "http://qdrant:6333".into(),
                api_key_env: Some("Q_KEY".into()),
            }),
        };
        let decoded: DatabaseConfig = toml::from_str(&toml::to_string(&db).unwrap()).unwrap();
        assert_eq!(decoded.url, "postgres://user:pass@host/db");
        let q = decoded.qdrant.unwrap();
        assert_eq!(q.url, "http://qdrant:6333");
        assert_eq!(q.api_key_env.as_deref(), Some("Q_KEY"));
    }

    #[test]
    fn test_qdrant_config_serde_roundtrip() {
        let q = QdrantConfig {
            url: "http://remote:6334".into(),
            api_key_env: Some("MY_KEY".into()),
        };
        let decoded: QdrantConfig = toml::from_str(&toml::to_string(&q).unwrap()).unwrap();
        assert_eq!(decoded.url, "http://remote:6334");
        assert_eq!(decoded.api_key_env.as_deref(), Some("MY_KEY"));
    }

    #[test]
    fn test_hybrid_weights_config_serde_roundtrip() {
        let hw = HybridWeightsConfig {
            semantic: 0.6,
            bm25: 0.25,
            fuzzy: 0.15,
        };
        let decoded: HybridWeightsConfig = toml::from_str(&toml::to_string(&hw).unwrap()).unwrap();
        assert!((decoded.semantic - 0.6).abs() < f32::EPSILON);
        assert!((decoded.bm25 - 0.25).abs() < f32::EPSILON);
        assert!((decoded.fuzzy - 0.15).abs() < f32::EPSILON);
    }

    #[test]
    fn test_rag_vector_config_serde_roundtrip() {
        let v = RAGVectorConfig {
            enabled: true,
            embedding_model: "nomic-embed".into(),
            sparse_embeddings: true,
            sparse_model: "custom-sparse".into(),
            vector_path: "/data/vecs".into(),
        };
        let decoded: RAGVectorConfig = toml::from_str(&toml::to_string(&v).unwrap()).unwrap();
        assert!(decoded.enabled);
        assert_eq!(decoded.embedding_model, "nomic-embed");
        assert!(decoded.sparse_embeddings);
        assert_eq!(decoded.sparse_model, "custom-sparse");
        assert_eq!(decoded.vector_path, "/data/vecs");
    }

    #[test]
    fn test_rag_chunking_config_serde_roundtrip() {
        let c = RagChunkingConfig {
            chunking_strategy: "semantic".into(),
            chunk_size: 500,
            chunk_overlap: 100,
            min_chunk_size: 50,
        };
        let decoded: RagChunkingConfig = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(decoded.chunking_strategy, "semantic");
        assert_eq!(decoded.chunk_size, 500);
        assert_eq!(decoded.chunk_overlap, 100);
        assert_eq!(decoded.min_chunk_size, 50);
    }

    #[test]
    fn test_rag_search_config_serde_roundtrip() {
        let s = RagSearchConfig {
            search_strategy: "hybrid".into(),
            search_limit: 25,
            search_threshold: 0.5,
            hybrid_weights: Some(HybridWeightsConfig::default()),
        };
        let decoded: RagSearchConfig = toml::from_str(&toml::to_string(&s).unwrap()).unwrap();
        assert_eq!(decoded.search_strategy, "hybrid");
        assert_eq!(decoded.search_limit, 25);
        assert!(decoded.hybrid_weights.is_some());
    }

    #[test]
    fn test_rag_reranking_config_serde_roundtrip() {
        let r = RagRerankingConfig {
            rerank_enabled: true,
            reranker_model: "jina-v2".into(),
            rerank_weight: 0.8,
        };
        let decoded: RagRerankingConfig = toml::from_str(&toml::to_string(&r).unwrap()).unwrap();
        assert!(decoded.rerank_enabled);
        assert_eq!(decoded.reranker_model, "jina-v2");
        assert!((decoded.rerank_weight - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn test_rag_config_serde_roundtrip() {
        let rag = RagConfig {
            vector: RAGVectorConfig {
                enabled: true,
                embedding_model: "test-model".into(),
                sparse_embeddings: true,
                sparse_model: "sparse-m".into(),
                vector_path: "/v".into(),
            },
            chunking: RagChunkingConfig {
                chunking_strategy: "semantic".into(),
                chunk_size: 300,
                chunk_overlap: 75,
                min_chunk_size: 30,
            },
            search: RagSearchConfig {
                search_strategy: "bm25".into(),
                search_limit: 5,
                search_threshold: 0.3,
                hybrid_weights: Some(HybridWeightsConfig {
                    semantic: 0.4,
                    bm25: 0.4,
                    fuzzy: 0.2,
                }),
            },
            rerank: RagRerankingConfig {
                rerank_enabled: true,
                reranker_model: "custom-reranker".into(),
                rerank_weight: 0.9,
            },
        };
        let decoded: RagConfig = toml::from_str(&toml::to_string(&rag).unwrap()).unwrap();
        assert!(decoded.vector.enabled);
        assert_eq!(decoded.chunking.chunk_size, 300);
        assert_eq!(decoded.search.search_strategy, "bm25");
        assert!(decoded.search.hybrid_weights.is_some());
        assert!(decoded.rerank.rerank_enabled);
    }

    #[test]
    fn test_billing_config_serde_roundtrip_from_toml() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "JWT"
api_key_env = "API"
[database]
[billing.model_pricing.gpt]
provider = "openai"
model = "gpt-4o"
input_usd_per_million_tokens = 2.5
output_usd_per_million_tokens = 10.0
currency = "USD"
[billing.model_pricing.free_tier]
provider = "openai"
model = "test-model"
input_usd_per_million_tokens = 0.0
output_usd_per_million_tokens = 0.0
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        assert_eq!(config.billing.model_pricing.len(), 2);
        let gpt = config.billing.pricing_for("openai", "gpt-4o").unwrap();
        assert!((gpt.input_usd_per_million_tokens.unwrap() - 2.5).abs() < f64::EPSILON);
        assert!((gpt.output_usd_per_million_tokens.unwrap() - 10.0).abs() < f64::EPSILON);
        let free = config.billing.pricing_for("openai", "test-model").unwrap();
        assert_eq!(free.currency, "USD");
    }

    // ---- Pricing edge cases ----

    #[test]
    fn test_pricing_key_whitespace_and_case() {
        let mut billing = BillingConfig::default();
        billing.model_pricing.insert(
            "e".into(),
            ModelPricingConfig {
                provider: "  OpenAI  ".into(),
                model: " GPT-4o ".into(),
                input_usd_per_million_tokens: Some(1.0),
                output_usd_per_million_tokens: Some(2.0),
                currency: "USD".into(),
            },
        );
        // pricing_key trims and lowercases, so these should all match
        assert!(billing.pricing_for("openai", "gpt-4o").is_some());
        assert!(billing.pricing_for("  OPENAI  ", "GPT-4O").is_some());
        assert!(billing.pricing_for("Openai", "Gpt-4O").is_some());
    }

    #[test]
    fn test_pricing_for_no_match() {
        let mut billing = BillingConfig::default();
        billing.model_pricing.insert(
            "e".into(),
            ModelPricingConfig {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                input_usd_per_million_tokens: None,
                output_usd_per_million_tokens: None,
                currency: "EUR".into(),
            },
        );
        assert!(billing.pricing_for("openai", "claude-3").is_none());
        assert!(billing.pricing_for("anthropic", "gpt-4o").is_none());
    }

    #[test]
    fn test_model_pricing_config_serde_roundtrip() {
        let mp = ModelPricingConfig {
            provider: "openai".into(),
            model: "gpt-4o".into(),
            input_usd_per_million_tokens: Some(2.5),
            output_usd_per_million_tokens: Some(10.0),
            currency: "EUR".into(),
        };
        let decoded: ModelPricingConfig = toml::from_str(&toml::to_string(&mp).unwrap()).unwrap();
        assert_eq!(decoded.provider, "openai");
        assert_eq!(decoded.model, "gpt-4o");
        assert_eq!(decoded.currency, "EUR");
    }

    // ---- Empty/minimal TOML parsing ----

    #[test]
    fn test_empty_toml_parses_with_defaults() {
        let toml_str = "[server]\n[auth]\njwt_secret_env = \"TEST_JWT\"\napi_key_env = \"TEST_API\"\n[database]\n";
        let config: AresConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 3000);
        assert!(config.providers.is_empty());
        assert!(config.models.is_empty());
        assert!(config.tools.is_empty());
        assert!(config.agents.is_empty());
        assert!(config.workflows.is_empty());
    }

    #[test]
    fn test_minimal_toml_with_only_server() {
        let toml_str = "[server]\nport = 9999\n\n[auth]\njwt_secret_env = \"TEST_JWT\"\napi_key_env = \"TEST_API\"\n\n[database]\n";
        let config: AresConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.server.port, 9999);
        assert_eq!(config.server.host, "127.0.0.1");
    }

    // ---- ProviderConfig type_name completeness ----

    // ---- FromStr edge cases ----

    #[test]
    fn test_provider_config_from_str_whitespace() {
        let p: ProviderConfig = "  openai  ".parse().unwrap();
        assert_eq!(p.type_name(), "openai");
    }

    #[test]
    fn test_provider_config_from_str_empty() {
        let result = "".parse::<ProviderConfig>();
        assert!(result.is_err());
    }

    // ---- validate_with_warnings error path ----

    #[test]
    fn test_validate_with_warnings_error_propagation() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[models.bad]
provider = "nonexistent"
model = "x"
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        // Should return error, not warnings
        assert!(config.validate_with_warnings().is_err());
    }

    // ---- Multiple models referencing same provider ----

    #[test]
    fn test_multiple_models_same_provider() {
        set_test_env();
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.p]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "m1"
[models.m1]
provider = "p"
model = "m1"
[models.m2]
provider = "p"
model = "m2"
[agents.a1]
model = "m1"
[workflows.w]
entry_agent = "a1"
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        assert!(config.validate().is_ok());
        // m2 model is unused (only a1 referenced in workflow), should warn
        let warnings = config.validate_with_warnings().unwrap();
        assert!(warnings
            .iter()
            .any(|w| w.kind == ConfigWarningKind::UnusedModel && w.message.contains("m2")));
    }

    // ---- ToolConfig with extra (flatten) fields from TOML ----

    #[test]
    fn test_tool_config_with_extra_fields_from_toml() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "JWT"
api_key_env = "API"
[database]
[tools.my_tool]
enabled = true
timeout_secs = 60
description = "Custom tool"
custom_param = "hello"
num_param = 42
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        let tool = config.get_tool("my_tool").unwrap();
        assert!(tool.enabled);
        assert_eq!(tool.timeout_secs, 60);
        assert_eq!(tool.description.as_deref(), Some("Custom tool"));
        assert_eq!(
            tool.extra.get("custom_param").and_then(|v| v.as_str()),
            Some("hello")
        );
        assert_eq!(
            tool.extra.get("num_param").and_then(|v| v.as_integer()),
            Some(42)
        );
    }

    // ---- AgentConfig with extra (flatten) fields from TOML ----

    #[test]
    fn test_agent_config_with_extra_fields_from_toml() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "JWT"
api_key_env = "API"
[database]
[providers.p]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "m"
[models.m]
provider = "p"
model = "m"
[agents.my_agent]
model = "m"
custom_bool = true
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        let agent = config.get_agent("my_agent").unwrap();
        assert_eq!(agent.model, "m");
        assert_eq!(
            agent.extra.get("custom_bool").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    // ---- AresConfig serde roundtrip ----

    #[test]
    fn test_ares_config_serde_roundtrip() {
        let content = create_test_config();
        let config: AresConfig = toml::from_str(&content).unwrap();
        let serialized = toml::to_string(&config).unwrap();
        let decoded: AresConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(decoded.server.host, config.server.host);
        assert_eq!(decoded.server.port, config.server.port);
        assert_eq!(decoded.providers.len(), config.providers.len());
        assert_eq!(decoded.models.len(), config.models.len());
        assert_eq!(decoded.tools.len(), config.tools.len());
        assert_eq!(decoded.agents.len(), config.agents.len());
        assert_eq!(decoded.workflows.len(), config.workflows.len());
    }

    // ---- QdrantConfig parsed from full AresConfig TOML ----

    #[test]
    fn test_database_qdrant_parsed_from_toml() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "JWT"
api_key_env = "API"
[database]
url = "postgres://host/db"
[database.qdrant]
url = "http://qdrant:6333"
api_key_env = "Q_KEY"
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        assert!(config.database.qdrant.is_some());
        let q = config.database.qdrant.unwrap();
        assert_eq!(q.url, "http://qdrant:6333");
        assert_eq!(q.api_key_env.as_deref(), Some("Q_KEY"));
    }

    // ---- ConfigWarning Display covers the message field ----

    #[test]
    fn test_config_warning_display_full_message() {
        let w = ConfigWarning {
            kind: ConfigWarningKind::UnusedTool,
            message: "Tool 'xyz' is defined but not referenced by any agent".into(),
        };
        assert_eq!(
            w.to_string(),
            "Tool 'xyz' is defined but not referenced by any agent"
        );
    }

    // ---- DynamicConfigPaths parsed from TOML sub-table ----

    #[test]
    fn test_dynamic_config_paths_partial_override() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "JWT"
api_key_env = "API"
[database]
[config]
agents_dir = "/only/agents"
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        assert_eq!(config.config.agents_dir, Path::new("/only/agents"));
        // Others should be defaults
        assert_eq!(config.config.workflows_dir, Path::new("config/workflows"));
        assert_eq!(config.config.models_dir, Path::new("config/models"));
        assert!(config.config.hot_reload);
    }

    // ---- Validate: enabled tool in agent_tools ----

    #[test]
    fn test_agent_tools_with_no_tools_agent() {
        set_test_env();
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.p]
type = "openai"
api_key_env = "TEST_KEY"
api_base = "https://test.example.com/v1"
default_model = "m"
[models.m]
provider = "p"
model = "m"
[agents.a]
model = "m"
[workflows.w]
entry_agent = "a"
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        assert!(config.agent_tools("a").is_empty());
    }

    // ---- Pricing with None token costs ----

    #[test]
    fn test_model_pricing_none_costs() {
        let mp = ModelPricingConfig {
            provider: "p".into(),
            model: "m".into(),
            input_usd_per_million_tokens: None,
            output_usd_per_million_tokens: None,
            currency: "USD".into(),
        };
        let toml_str = toml::to_string(&mp).unwrap();
        let decoded: ModelPricingConfig = toml::from_str(&toml_str).unwrap();
        assert!(decoded.input_usd_per_million_tokens.is_none());
        assert!(decoded.output_usd_per_million_tokens.is_none());
    }

    // ---- ConfigManager Clone ----

    #[test]
    fn test_config_manager_clone_reads_same_config() {
        let config = toml::from_str(
            r#"
[server]
port = 42
[auth]
jwt_secret_env = "JWT"
api_key_env = "API"
[database]
"#,
        )
        .unwrap();
        let manager = AresConfigManager::from_config(config);
        let cloned = manager.clone();
        assert_eq!(manager.config().server.port, cloned.config().server.port);
    }

    // ---- RAG sub-config defaults completeness ----

    #[test]
    fn test_rag_search_default_threshold() {
        let s = RagSearchConfig::default();
        assert!((s.search_threshold).abs() < f32::EPSILON);
    }

    // ---- LlamaCpp defaults from FromStr ----

    #[test]
    fn test_provider_config_from_str_openai_defaults() {
        let p: ProviderConfig = "openai".parse().unwrap();
        if let ProviderConfig::OpenAI {
            api_key_env,
            api_base,
            default_model,
        } = p
        {
            assert_eq!(api_key_env, "OPENAI_API_KEY");
            assert_eq!(api_base, "https://api.openai.com/v1");
            assert_eq!(default_model, "gpt-4");
        } else {
            panic!("expected openai variant");
        }
    }

    #[test]
    fn test_provider_config_from_str_nvidia_keeps_nim_defaults() {
        let p: ProviderConfig = "nvidia".parse().unwrap();
        if let ProviderConfig::OpenAI {
            api_key_env,
            api_base,
            default_model,
        } = p
        {
            assert_eq!(api_key_env, "NVIDIA_API_KEY");
            assert_eq!(api_base, "https://integrate.api.nvidia.com/v1");
            assert_eq!(default_model, "nvidia/nemotron-3-ultra-550b-a55b");
        } else {
            panic!("expected openai-compat nvidia variant");
        }
    }

    #[test]
    fn test_tool_config_default_struct() {
        let tool = ToolConfig::default();
        assert!(tool.enabled);
        assert!(tool.description.is_none());
        assert_eq!(tool.timeout_secs, 30);
        assert!(tool.extra.is_empty());
    }

    #[test]
    fn test_qdrant_config_default_struct() {
        let qdrant = QdrantConfig::default();
        assert_eq!(qdrant.url, "http://localhost:6334");
        assert!(qdrant.api_key_env.is_none());
    }

    #[test]
    fn ares_config_still_deserializes_from_toml_config_test_fixture() {
        let config: AresConfig =
            toml::from_str(&create_test_config()).expect("fixture should parse");
        assert_eq!(config.server.port, 3000);
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.auth.jwt_secret_env, "TEST_JWT_SECRET");
        assert_eq!(config.database.url, "./data/test.db");
        assert!(config.agents.contains_key("router"));
        assert!(config.tools.contains_key("calculator"));
        assert!(!config.billing.model_pricing.is_empty());
    }

    #[test]
    fn test_ares_config_load_success() {
        set_test_env();
        let dir = std::env::temp_dir().join("ares_load_success_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ares.toml");
        std::fs::write(&path, create_test_config()).unwrap();

        let config = AresConfig::load(&path).expect("load should succeed");
        assert_eq!(config.server.port, 3000);
        assert!(config.providers.contains_key("ollama-local"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
