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
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Root configuration structure loaded from ares.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AresConfig {
    pub server: ServerConfig,
    pub auth: AuthConfig,
    pub database: DatabaseConfig,

    /// Named LLM provider configurations
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    /// Named model configurations that reference providers
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,

    /// Tool configurations
    #[serde(default)]
    pub tools: HashMap<String, ToolConfig>,

    /// Agent configurations
    #[serde(default)]
    pub agents: HashMap<String, AgentConfig>,

    /// Workflow configurations
    #[serde(default)]
    pub workflows: HashMap<String, WorkflowConfig>,

    /// RAG configuration
    #[serde(default)]
    pub rag: RagConfig,
}

// ============= Server Configuration =============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    3000
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            log_level: default_log_level(),
        }
    }
}

// ============= Authentication Configuration =============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Environment variable name containing the JWT secret
    pub jwt_secret_env: String,

    #[serde(default = "default_jwt_access_expiry")]
    pub jwt_access_expiry: i64,

    #[serde(default = "default_jwt_refresh_expiry")]
    pub jwt_refresh_expiry: i64,

    /// Environment variable name containing the API key
    pub api_key_env: String,
}

fn default_jwt_access_expiry() -> i64 {
    900
}

fn default_jwt_refresh_expiry() -> i64 {
    604800
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret_env: "JWT_SECRET".to_string(),
            jwt_access_expiry: default_jwt_access_expiry(),
            jwt_refresh_expiry: default_jwt_refresh_expiry(),
            api_key_env: "API_KEY".to_string(),
        }
    }
}

// ============= Database Configuration =============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Local database URL/path
    #[serde(default = "default_database_url")]
    pub url: String,

    /// Environment variable for Turso URL (optional cloud config)
    pub turso_url_env: Option<String>,

    /// Environment variable for Turso auth token
    pub turso_token_env: Option<String>,

    /// Qdrant configuration (optional)
    pub qdrant: Option<QdrantConfig>,
}

fn default_database_url() -> String {
    "./data/ares.db".to_string()
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: default_database_url(),
            turso_url_env: None,
            turso_token_env: None,
            qdrant: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QdrantConfig {
    #[serde(default = "default_qdrant_url")]
    pub url: String,

    /// Environment variable for Qdrant API key
    pub api_key_env: Option<String>,
}

fn default_qdrant_url() -> String {
    "http://localhost:6334".to_string()
}

impl Default for QdrantConfig {
    fn default() -> Self {
        Self {
            url: default_qdrant_url(),
            api_key_env: None,
        }
    }
}

// ============= Provider Configuration =============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ProviderConfig {
    Ollama {
        #[serde(default = "default_ollama_url")]
        base_url: String,
        default_model: String,
    },
    OpenAI {
        /// Environment variable containing API key
        api_key_env: String,
        #[serde(default = "default_openai_base")]
        api_base: String,
        default_model: String,
    },
    LlamaCpp {
        model_path: String,
        #[serde(default = "default_n_ctx")]
        n_ctx: u32,
        #[serde(default = "default_n_threads")]
        n_threads: u32,
        #[serde(default = "default_max_tokens")]
        max_tokens: u32,
    },
}

fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}

fn default_openai_base() -> String {
    "https://api.openai.com/v1".to_string()
}

fn default_n_ctx() -> u32 {
    4096
}

fn default_n_threads() -> u32 {
    4
}

fn default_max_tokens() -> u32 {
    512
}

// ============= Model Configuration =============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Reference to a provider name defined in [providers]
    pub provider: String,

    /// Model name/identifier to use with the provider
    pub model: String,

    #[serde(default = "default_temperature")]
    pub temperature: f32,

    #[serde(default = "default_model_max_tokens")]
    pub max_tokens: u32,

    /// Optional top_p value
    pub top_p: Option<f32>,

    /// Optional frequency penalty
    pub frequency_penalty: Option<f32>,

    /// Optional presence penalty
    pub presence_penalty: Option<f32>,
}

fn default_temperature() -> f32 {
    0.7
}

fn default_model_max_tokens() -> u32 {
    512
}

// ============= Tool Configuration =============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default)]
    pub description: Option<String>,

    #[serde(default = "default_tool_timeout")]
    pub timeout_secs: u64,

    /// Additional tool-specific configuration
    #[serde(flatten)]
    pub extra: HashMap<String, toml::Value>,
}

fn default_true() -> bool {
    true
}

fn default_tool_timeout() -> u64 {
    30
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            description: None,
            timeout_secs: default_tool_timeout(),
            extra: HashMap::new(),
        }
    }
}

// ============= Agent Configuration =============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Reference to a model name defined in [models]
    pub model: String,

    /// System prompt for the agent
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// List of tool names this agent can use
    #[serde(default)]
    pub tools: Vec<String>,

    /// Maximum tool calling iterations
    #[serde(default = "default_max_tool_iterations")]
    pub max_tool_iterations: usize,

    /// Whether to execute tools in parallel
    #[serde(default)]
    pub parallel_tools: bool,

    /// Additional agent-specific configuration
    #[serde(flatten)]
    pub extra: HashMap<String, toml::Value>,
}

fn default_max_tool_iterations() -> usize {
    10
}

// ============= Workflow Configuration =============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowConfig {
    /// Entry point agent
    pub entry_agent: String,

    /// Fallback agent if routing fails
    pub fallback_agent: Option<String>,

    /// Maximum depth for recursive workflows
    #[serde(default = "default_max_depth")]
    pub max_depth: u8,

    /// Maximum iterations for research workflows
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u8,

    /// Whether to use parallel subagents
    #[serde(default)]
    pub parallel_subagents: bool,
}

fn default_max_depth() -> u8 {
    3
}

fn default_max_iterations() -> u8 {
    5
}

// ============= RAG Configuration =============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagConfig {
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,

    #[serde(default = "default_chunk_size")]
    pub chunk_size: usize,

    #[serde(default = "default_chunk_overlap")]
    pub chunk_overlap: usize,
}

fn default_embedding_model() -> String {
    "BAAI/bge-small-en-v1.5".to_string()
}

fn default_chunk_size() -> usize {
    1000
}

fn default_chunk_overlap() -> usize {
    200
}

impl Default for RagConfig {
    fn default() -> Self {
        Self {
            embedding_model: default_embedding_model(),
            chunk_size: default_chunk_size(),
            chunk_overlap: default_chunk_overlap(),
        }
    }
}

// ============= Configuration Loading & Validation =============

/// Configuration warnings that don't prevent operation but may indicate issues
#[derive(Debug, Clone)]
pub struct ConfigWarning {
    pub kind: ConfigWarningKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigWarningKind {
    UnusedProvider,
    UnusedModel,
    UnusedTool,
    UnusedAgent,
}

impl std::fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Errors that can occur during configuration loading
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Configuration file not found: {0}")]
    FileNotFound(PathBuf),

    #[error("Failed to read configuration file: {0}")]
    ReadError(#[from] std::io::Error),

    #[error("Failed to parse TOML: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Environment variable '{0}' referenced in config is not set")]
    MissingEnvVar(String),

    #[error("Provider '{0}' referenced by model '{1}' does not exist")]
    MissingProvider(String, String),

    #[error("Model '{0}' referenced by agent '{1}' does not exist")]
    MissingModel(String, String),

    #[error("Agent '{0}' referenced by workflow '{1}' does not exist")]
    MissingAgent(String, String),

    #[error("Tool '{0}' referenced by agent '{1}' does not exist")]
    MissingTool(String, String),

    #[error("Circular reference detected: {0}")]
    CircularReference(String),

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

    /// Validate the configuration for internal consistency and env var availability
    pub fn validate(&self) -> Result<(), ConfigError> {
        // Validate auth env vars exist
        self.validate_env_var(&self.auth.jwt_secret_env)?;
        self.validate_env_var(&self.auth.api_key_env)?;

        // Validate database env vars if specified
        if let Some(ref env) = self.database.turso_url_env {
            self.validate_env_var(env)?;
        }
        if let Some(ref env) = self.database.turso_token_env {
            self.validate_env_var(env)?;
        }
        if let Some(ref qdrant) = self.database.qdrant
            && let Some(ref env) = qdrant.api_key_env
        {
            self.validate_env_var(env)?;
        }

        // Validate provider env vars
        for (name, provider) in &self.providers {
            match provider {
                ProviderConfig::OpenAI { api_key_env, .. } => {
                    self.validate_env_var(api_key_env)?;
                }
                ProviderConfig::LlamaCpp { model_path, .. } => {
                    // Validate model path exists
                    if !Path::new(model_path).exists() {
                        return Err(ConfigError::ValidationError(format!(
                            "LlamaCpp model path does not exist: {} (provider: {})",
                            model_path, name
                        )));
                    }
                }
                ProviderConfig::Ollama { .. } => {
                    // Ollama doesn't require validation - it's the default fallback
                }
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
        for (agent_name, agent_config) in &self.agents {
            if !self.models.contains_key(&agent_config.model) {
                return Err(ConfigError::MissingModel(
                    agent_config.model.clone(),
                    agent_name.clone(),
                ));
            }

            for tool_name in &agent_config.tools {
                if !self.tools.contains_key(tool_name) {
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

            if let Some(ref fallback) = workflow_config.fallback_agent
                && !self.agents.contains_key(fallback)
            {
                return Err(ConfigError::MissingAgent(
                    fallback.clone(),
                    workflow_name.clone(),
                ));
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
                if let Some(ref fallback) = workflow_config.fallback_agent
                    && fallback == &workflow_config.entry_agent
                {
                    return Err(ConfigError::CircularReference(format!(
                        "Workflow '{}' has entry_agent '{}' that equals fallback_agent",
                        workflow_name, workflow_config.entry_agent
                    )));
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

    /// Get the JWT secret from the environment
    pub fn jwt_secret(&self) -> Result<String, ConfigError> {
        self.resolve_env(&self.auth.jwt_secret_env)
            .ok_or_else(|| ConfigError::MissingEnvVar(self.auth.jwt_secret_env.clone()))
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

    /// Start watching for configuration file changes
    pub fn start_watching(&mut self) -> Result<(), ConfigError> {
        let (tx, mut rx) = mpsc::unbounded_channel::<()>();
        self.reload_tx = Some(tx.clone());

        let config_path = self.config_path.clone();
        let config_arc = Arc::clone(&self.config);

        // Create debounced file watcher
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    if event.kind.is_modify() || event.kind.is_create() {
                        // Send reload signal (debounced in the receiver)
                        let _ = tx.send(());
                    }
                }
                Err(e) => {
                    error!("Config watcher error: {:?}", e);
                }
            }
        })?;

        // Watch the config file's parent directory
        if let Some(parent) = self.config_path.parent() {
            watcher.watch(parent, RecursiveMode::NonRecursive)?;
        }

        *self.watcher.write() = Some(watcher);

        // Spawn reload handler with debouncing
        let config_path_clone = config_path.clone();
        tokio::spawn(async move {
            let mut last_reload = std::time::Instant::now();
            let debounce_duration = Duration::from_millis(500);

            while rx.recv().await.is_some() {
                // Debounce: only reload if enough time has passed
                if last_reload.elapsed() < debounce_duration {
                    continue;
                }

                // Wait a bit for file write to complete
                tokio::time::sleep(Duration::from_millis(100)).await;

                match AresConfig::load(&config_path_clone) {
                    Ok(new_config) => {
                        config_arc.store(Arc::new(new_config));
                        info!("Configuration hot-reloaded successfully");
                        last_reload = std::time::Instant::now();
                    }
                    Err(e) => {
                        warn!(
                            "Failed to hot-reload config: {}. Keeping previous config.",
                            e
                        );
                    }
                }
            }
        });

        info!("Configuration hot-reload watcher started");
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
        }
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
type = "ollama"
base_url = "http://localhost:11434"
default_model = "granite4:tiny-h"

[models.default]
provider = "ollama-local"
model = "granite4:tiny-h"
temperature = 0.7
max_tokens = 512

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
            std::env::set_var("TEST_JWT_SECRET", "test-secret-at-least-32-characters-long");
            std::env::set_var("TEST_API_KEY", "test-api-key");
        }

        let content = create_test_config();
        let config: AresConfig = toml::from_str(&content).expect("Failed to parse config");

        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 3000);
        assert!(config.providers.contains_key("ollama-local"));
        assert!(config.models.contains_key("default"));
        assert!(config.agents.contains_key("router"));
    }

    #[test]
    fn test_validation_missing_provider() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret");
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
            std::env::set_var("TEST_JWT_SECRET", "test-secret");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.test]
type = "ollama"
default_model = "granite4:tiny-h"
[agents.test]
model = "nonexistent"
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let result = config.validate();

        assert!(matches!(result, Err(ConfigError::MissingModel(_, _))));
    }

    #[test]
    fn test_validation_missing_tool() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.test]
type = "ollama"
default_model = "granite4:tiny-h"
[models.default]
provider = "test"
model = "granite4:tiny-h"
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
            std::env::set_var("TEST_JWT_SECRET", "test-secret");
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
        assert_eq!(config.database.url, "./data/ares.db");

        // RAG defaults
        assert_eq!(config.rag.embedding_model, "BAAI/bge-small-en-v1.5");
        assert_eq!(config.rag.chunk_size, 1000);
        assert_eq!(config.rag.chunk_overlap, 200);
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
            std::env::set_var("TEST_JWT_SECRET", "test-secret");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.test]
type = "ollama"
default_model = "granite4:tiny-h"
[models.default]
provider = "test"
model = "granite4:tiny-h"
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
            std::env::set_var("TEST_JWT_SECRET", "test-secret");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.used]
type = "ollama"
default_model = "granite4:tiny-h"
[providers.unused]
type = "ollama"
default_model = "granite4:tiny-h"
[models.default]
provider = "used"
model = "granite4:tiny-h"
[agents.router]
model = "default"
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let warnings = config.validate_with_warnings().unwrap();

        assert!(
            warnings.iter().any(
                |w| w.kind == ConfigWarningKind::UnusedProvider && w.message.contains("unused")
            )
        );
    }

    #[test]
    fn test_unused_model_warning() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.test]
type = "ollama"
default_model = "granite4:tiny-h"
[models.used]
provider = "test"
model = "granite4:tiny-h"
[models.unused]
provider = "test"
model = "other"
[agents.router]
model = "used"
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let warnings = config.validate_with_warnings().unwrap();

        assert!(
            warnings
                .iter()
                .any(|w| w.kind == ConfigWarningKind::UnusedModel && w.message.contains("unused"))
        );
    }

    #[test]
    fn test_unused_tool_warning() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.test]
type = "ollama"
default_model = "granite4:tiny-h"
[models.default]
provider = "test"
model = "granite4:tiny-h"
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

        assert!(
            warnings
                .iter()
                .any(|w| w.kind == ConfigWarningKind::UnusedTool
                    && w.message.contains("unused_tool"))
        );
    }

    #[test]
    fn test_unused_agent_warning() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.test]
type = "ollama"
default_model = "granite4:tiny-h"
[models.default]
provider = "test"
model = "granite4:tiny-h"
[agents.router]
model = "default"
[agents.orphaned]
model = "default"
[workflows.test_flow]
entry_agent = "router"
"#;

        let config: AresConfig = toml::from_str(content).unwrap();
        let warnings = config.validate_with_warnings().unwrap();

        assert!(warnings.iter().any(|w| w.kind == ConfigWarningKind::UnusedAgent && w.message.contains("orphaned")));
    }

    #[test]
    fn test_no_warnings_for_fully_connected_config() {
        // SAFETY: Tests are run single-threaded for env var safety
        unsafe {
            std::env::set_var("TEST_JWT_SECRET", "test-secret");
            std::env::set_var("TEST_API_KEY", "test-key");
        }

        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT_SECRET"
api_key_env = "TEST_API_KEY"
[database]
[providers.test]
type = "ollama"
default_model = "granite4:tiny-h"
[models.default]
provider = "test"
model = "granite4:tiny-h"
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
}
