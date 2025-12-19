//! TOON-based dynamic configuration for A.R.E.S
//!
//! This module handles hot-reloadable behavioral configuration:
//! - Agents
//! - Workflows
//! - Models
//! - Tools
//! - MCP servers
//!
//! # Architecture
//!
//! ARES uses a hybrid configuration approach:
//! - **TOML** (`ares.toml`): Static infrastructure config (server, auth, database, providers)
//! - **TOON** (`config/*.toon`): Dynamic behavioral config (agents, workflows, models, tools, MCPs)
//!
//! This separation achieves:
//! 1. Separation of concerns: Infrastructure vs. behavior
//! 2. Token efficiency: TOON reduces LLM context usage by 30-60%
//! 3. Hot-reloadability: Behavioral configs can change without restarts
//! 4. LLM-friendliness: TOON is optimized for AI consumption
//!
//! # Example Agent Config (`config/agents/router.toon`)
//!
//! ```toon
//! name: router
//! model: fast
//! max_tool_iterations: 1
//! parallel_tools: false
//! tools[0]:
//! system_prompt: |
//!   You are a routing agent...
//! ```

use arc_swap::ArcSwap;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use toon_format::{decode_default, encode_default, ToonError};
use tracing::{debug, error, info, warn};

// ============= Agent Configuration =============

/// Configuration for an AI agent loaded from TOON files
///
/// Agents are the core behavioral units in ARES. Each agent has:
/// - A model reference (defined in `config/models/*.toon`)
/// - A system prompt defining its behavior
/// - Optional tools it can use
/// - Iteration limits for tool calling
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToonAgentConfig {
    /// Unique identifier for the agent
    pub name: String,

    /// Reference to a model name defined in `config/models/`
    pub model: String,

    /// System prompt defining agent behavior
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// List of tool names this agent can use (defined in `config/tools/`)
    #[serde(default)]
    pub tools: Vec<String>,

    /// Maximum tool calling iterations before returning
    #[serde(default = "default_max_tool_iterations")]
    pub max_tool_iterations: usize,

    /// Whether to execute multiple tool calls in parallel
    #[serde(default)]
    pub parallel_tools: bool,

    /// Additional agent-specific configuration (extensible)
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

fn default_max_tool_iterations() -> usize {
    10
}

impl ToonAgentConfig {
    /// Create a new agent config with required fields
    pub fn new(name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            model: model.into(),
            system_prompt: None,
            tools: Vec::new(),
            max_tool_iterations: default_max_tool_iterations(),
            parallel_tools: false,
            extra: HashMap::new(),
        }
    }

    /// Set the system prompt
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Set the tools list
    pub fn with_tools(mut self, tools: Vec<String>) -> Self {
        self.tools = tools;
        self
    }

    /// Encode this config to TOON format
    pub fn to_toon(&self) -> Result<String, ToonConfigError> {
        encode_default(self).map_err(ToonConfigError::from)
    }

    /// Parse an agent config from TOON format
    pub fn from_toon(toon: &str) -> Result<Self, ToonConfigError> {
        decode_default(toon).map_err(ToonConfigError::from)
    }
}

// ============= Model Configuration =============

/// Configuration for an LLM model loaded from TOON files
///
/// Models reference providers defined in `ares.toml` and specify
/// inference parameters like temperature and token limits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToonModelConfig {
    /// Unique identifier for the model configuration
    pub name: String,

    /// Reference to a provider name defined in `ares.toml` [providers.*]
    pub provider: String,

    /// Model name/identifier to use with the provider (e.g., "gpt-4", "ministral-3:3b")
    pub model: String,

    /// Sampling temperature (0.0 = deterministic, 1.0+ = creative)
    #[serde(default = "default_temperature")]
    pub temperature: f32,

    /// Maximum tokens to generate
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,

    /// Optional nucleus sampling parameter
    #[serde(default)]
    pub top_p: Option<f32>,

    /// Optional frequency penalty (-2.0 to 2.0)
    #[serde(default)]
    pub frequency_penalty: Option<f32>,

    /// Optional presence penalty (-2.0 to 2.0)
    #[serde(default)]
    pub presence_penalty: Option<f32>,
}

fn default_temperature() -> f32 {
    0.7
}

fn default_max_tokens() -> u32 {
    512
}

impl ToonModelConfig {
    /// Create a new model config with required fields
    pub fn new(
        name: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            provider: provider.into(),
            model: model.into(),
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
        }
    }

    /// Encode this config to TOON format
    pub fn to_toon(&self) -> Result<String, ToonConfigError> {
        encode_default(self).map_err(ToonConfigError::from)
    }

    /// Parse a model config from TOON format
    pub fn from_toon(toon: &str) -> Result<Self, ToonConfigError> {
        decode_default(toon).map_err(ToonConfigError::from)
    }
}

// ============= Tool Configuration =============

/// Configuration for a tool loaded from TOON files
///
/// Tools provide external capabilities to agents (calculator, web search, etc.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToonToolConfig {
    /// Unique identifier for the tool
    pub name: String,

    /// Whether this tool is currently enabled
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Human-readable description of what the tool does
    #[serde(default)]
    pub description: Option<String>,

    /// Timeout in seconds for tool execution
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Additional tool-specific configuration
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

fn default_true() -> bool {
    true
}

fn default_timeout() -> u64 {
    30
}

impl ToonToolConfig {
    /// Create a new tool config with required fields
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            enabled: default_true(),
            description: None,
            timeout_secs: default_timeout(),
            extra: HashMap::new(),
        }
    }

    /// Encode this config to TOON format
    pub fn to_toon(&self) -> Result<String, ToonConfigError> {
        encode_default(self).map_err(ToonConfigError::from)
    }

    /// Parse a tool config from TOON format
    pub fn from_toon(toon: &str) -> Result<Self, ToonConfigError> {
        decode_default(toon).map_err(ToonConfigError::from)
    }
}

// ============= Workflow Configuration =============

/// Configuration for a workflow loaded from TOON files
///
/// Workflows define how agents work together to handle complex requests.
/// They specify entry points, fallbacks, and iteration limits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToonWorkflowConfig {
    /// Unique identifier for the workflow
    pub name: String,

    /// The agent that first receives requests
    pub entry_agent: String,

    /// Agent to use if routing/entry fails
    #[serde(default)]
    pub fallback_agent: Option<String>,

    /// Maximum depth for recursive agent calls
    #[serde(default = "default_max_depth")]
    pub max_depth: u8,

    /// Maximum total iterations across all agents
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u8,

    /// Whether to run subagents in parallel when possible
    #[serde(default)]
    pub parallel_subagents: bool,
}

fn default_max_depth() -> u8 {
    3
}

fn default_max_iterations() -> u8 {
    5
}

impl ToonWorkflowConfig {
    /// Create a new workflow config with required fields
    pub fn new(name: impl Into<String>, entry_agent: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            entry_agent: entry_agent.into(),
            fallback_agent: None,
            max_depth: default_max_depth(),
            max_iterations: default_max_iterations(),
            parallel_subagents: false,
        }
    }

    /// Encode this config to TOON format
    pub fn to_toon(&self) -> Result<String, ToonConfigError> {
        encode_default(self).map_err(ToonConfigError::from)
    }

    /// Parse a workflow config from TOON format
    pub fn from_toon(toon: &str) -> Result<Self, ToonConfigError> {
        decode_default(toon).map_err(ToonConfigError::from)
    }
}

// ============= MCP Server Configuration =============

/// Configuration for an MCP (Model Context Protocol) server
///
/// MCP servers provide additional capabilities to agents via a standardized protocol.
/// See: <https://modelcontextprotocol.io/>
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToonMcpConfig {
    /// Unique identifier for the MCP server
    pub name: String,

    /// Whether this MCP server is currently enabled
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Command to run the MCP server (e.g., "npx", "python")
    pub command: String,

    /// Arguments to pass to the command
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables to set for the MCP server
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Timeout in seconds for MCP operations
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

impl ToonMcpConfig {
    /// Create a new MCP config with required fields
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            enabled: default_true(),
            command: command.into(),
            args: Vec::new(),
            env: HashMap::new(),
            timeout_secs: default_timeout(),
        }
    }

    /// Encode this config to TOON format
    pub fn to_toon(&self) -> Result<String, ToonConfigError> {
        encode_default(self).map_err(ToonConfigError::from)
    }

    /// Parse an MCP config from TOON format
    pub fn from_toon(toon: &str) -> Result<Self, ToonConfigError> {
        decode_default(toon).map_err(ToonConfigError::from)
    }
}

// ============= Dynamic Config Aggregate =============

/// Aggregated dynamic configuration from all TOON files
///
/// This struct holds all behavioral configuration loaded from the
/// `config/` directory tree. It is wrapped in `ArcSwap` for
/// lock-free concurrent access with atomic updates during hot-reload.
#[derive(Debug, Clone, Default)]
pub struct DynamicConfig {
    /// Agent configurations keyed by name
    pub agents: HashMap<String, ToonAgentConfig>,
    /// Model configurations keyed by name
    pub models: HashMap<String, ToonModelConfig>,
    /// Tool configurations keyed by name
    pub tools: HashMap<String, ToonToolConfig>,
    /// Workflow configurations keyed by name
    pub workflows: HashMap<String, ToonWorkflowConfig>,
    /// MCP server configurations keyed by name
    pub mcps: HashMap<String, ToonMcpConfig>,
}

impl DynamicConfig {
    /// Load all TOON configs from directories
    pub fn load(
        agents_dir: &Path,
        models_dir: &Path,
        tools_dir: &Path,
        workflows_dir: &Path,
        mcps_dir: &Path,
    ) -> Result<Self, ToonConfigError> {
        let agents = load_configs_from_dir::<ToonAgentConfig>(agents_dir, "agents")?;
        let models = load_configs_from_dir::<ToonModelConfig>(models_dir, "models")?;
        let tools = load_configs_from_dir::<ToonToolConfig>(tools_dir, "tools")?;
        let workflows = load_configs_from_dir::<ToonWorkflowConfig>(workflows_dir, "workflows")?;
        let mcps = load_configs_from_dir::<ToonMcpConfig>(mcps_dir, "mcps")?;

        info!(
            "Loaded dynamic config: {} agents, {} models, {} tools, {} workflows, {} mcps",
            agents.len(),
            models.len(),
            tools.len(),
            workflows.len(),
            mcps.len()
        );

        Ok(Self {
            agents,
            models,
            tools,
            workflows,
            mcps,
        })
    }

    /// Get an agent config by name
    pub fn get_agent(&self, name: &str) -> Option<&ToonAgentConfig> {
        self.agents.get(name)
    }

    /// Get a model config by name
    pub fn get_model(&self, name: &str) -> Option<&ToonModelConfig> {
        self.models.get(name)
    }

    /// Get a tool config by name
    pub fn get_tool(&self, name: &str) -> Option<&ToonToolConfig> {
        self.tools.get(name)
    }

    /// Get a workflow config by name
    pub fn get_workflow(&self, name: &str) -> Option<&ToonWorkflowConfig> {
        self.workflows.get(name)
    }

    /// Get an MCP config by name
    pub fn get_mcp(&self, name: &str) -> Option<&ToonMcpConfig> {
        self.mcps.get(name)
    }

    /// Get all agent names
    pub fn agent_names(&self) -> Vec<&str> {
        self.agents.keys().map(|s| s.as_str()).collect()
    }

    /// Get all model names
    pub fn model_names(&self) -> Vec<&str> {
        self.models.keys().map(|s| s.as_str()).collect()
    }

    /// Get all tool names
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    /// Get all workflow names
    pub fn workflow_names(&self) -> Vec<&str> {
        self.workflows.keys().map(|s| s.as_str()).collect()
    }

    /// Get all MCP names
    pub fn mcp_names(&self) -> Vec<&str> {
        self.mcps.keys().map(|s| s.as_str()).collect()
    }

    /// Validate the configuration for internal consistency
    pub fn validate(&self) -> Result<Vec<ConfigWarning>, ToonConfigError> {
        let mut warnings = Vec::new();

        // Validate agent -> model references
        for (agent_name, agent) in &self.agents {
            if !self.models.contains_key(&agent.model) {
                return Err(ToonConfigError::Validation(format!(
                    "Agent '{}' references unknown model '{}'",
                    agent_name, agent.model
                )));
            }

            // Validate agent -> tools references
            for tool_name in &agent.tools {
                if !self.tools.contains_key(tool_name) {
                    return Err(ToonConfigError::Validation(format!(
                        "Agent '{}' references unknown tool '{}'",
                        agent_name, tool_name
                    )));
                }
            }
        }

        // Validate workflow -> agent references
        for (workflow_name, workflow) in &self.workflows {
            if !self.agents.contains_key(&workflow.entry_agent) {
                return Err(ToonConfigError::Validation(format!(
                    "Workflow '{}' references unknown entry agent '{}'",
                    workflow_name, workflow.entry_agent
                )));
            }

            if let Some(ref fallback) = workflow.fallback_agent {
                if !self.agents.contains_key(fallback) {
                    return Err(ToonConfigError::Validation(format!(
                        "Workflow '{}' references unknown fallback agent '{}'",
                        workflow_name, fallback
                    )));
                }
            }
        }

        // Check for unused models
        let used_models: std::collections::HashSet<_> =
            self.agents.values().map(|a| &a.model).collect();
        for model_name in self.models.keys() {
            if !used_models.contains(model_name) {
                warnings.push(ConfigWarning {
                    kind: WarningKind::UnusedModel,
                    message: format!("Model '{}' is not used by any agent", model_name),
                });
            }
        }

        // Check for unused tools
        let used_tools: std::collections::HashSet<_> =
            self.agents.values().flat_map(|a| a.tools.iter()).collect();
        for tool_name in self.tools.keys() {
            if !used_tools.contains(tool_name) {
                warnings.push(ConfigWarning {
                    kind: WarningKind::UnusedTool,
                    message: format!("Tool '{}' is not used by any agent", tool_name),
                });
            }
        }

        Ok(warnings)
    }
}

// ============= Config Loading Helpers =============

/// Trait for config types that have a name field.
///
/// All TOON config types must implement this trait to enable
/// automatic keying by name when loading from directories.
pub trait HasName {
    /// Returns the unique name/identifier of this configuration.
    fn name(&self) -> &str;
}

impl HasName for ToonAgentConfig {
    fn name(&self) -> &str {
        &self.name
    }
}

impl HasName for ToonModelConfig {
    fn name(&self) -> &str {
        &self.name
    }
}

impl HasName for ToonToolConfig {
    fn name(&self) -> &str {
        &self.name
    }
}

impl HasName for ToonWorkflowConfig {
    fn name(&self) -> &str {
        &self.name
    }
}

impl HasName for ToonMcpConfig {
    fn name(&self) -> &str {
        &self.name
    }
}

/// Load all .toon files from a directory into a HashMap keyed by name
fn load_configs_from_dir<T>(
    dir: &Path,
    config_type: &str,
) -> Result<HashMap<String, T>, ToonConfigError>
where
    T: for<'de> Deserialize<'de> + HasName,
{
    let mut configs = HashMap::new();

    if !dir.exists() {
        debug!("Config directory does not exist: {:?}", dir);
        return Ok(configs);
    }

    let entries = fs::read_dir(dir).map_err(|e| {
        ToonConfigError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to read {} directory {:?}: {}", config_type, dir, e),
        ))
    })?;

    for entry in entries {
        let entry = entry.map_err(ToonConfigError::Io)?;
        let path = entry.path();

        // Only process .toon files
        if path.extension().and_then(|e| e.to_str()) != Some("toon") {
            continue;
        }

        match load_toon_file::<T>(&path) {
            Ok(config) => {
                let name = config.name().to_string();
                debug!("Loaded {} config: {}", config_type, name);
                configs.insert(name, config);
            }
            Err(e) => {
                warn!("Failed to load {} from {:?}: {}", config_type, path, e);
            }
        }
    }

    Ok(configs)
}

/// Load a single TOON file and deserialize it
fn load_toon_file<T>(path: &Path) -> Result<T, ToonConfigError>
where
    T: for<'de> Deserialize<'de>,
{
    let content = fs::read_to_string(path).map_err(|e| {
        ToonConfigError::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to read {:?}: {}", path, e),
        ))
    })?;

    decode_default(&content)
        .map_err(|e| ToonConfigError::Parse(format!("Failed to parse {:?}: {}", path, e)))
}

// ============= Error Types =============

/// Errors that can occur during TOON configuration loading.
#[derive(Debug, thiserror::Error)]
pub enum ToonConfigError {
    /// An I/O error occurred while reading configuration files.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Failed to parse TOON format content.
    #[error("TOON parse error: {0}")]
    Parse(String),

    /// Configuration validation failed (e.g., missing references).
    #[error("Validation error: {0}")]
    Validation(String),

    /// An error occurred while watching configuration files for changes.
    #[error("Watch error: {0}")]
    Watch(#[from] notify::Error),
}

impl From<ToonError> for ToonConfigError {
    fn from(e: ToonError) -> Self {
        ToonConfigError::Parse(e.to_string())
    }
}

/// Non-fatal configuration warnings.
#[derive(Debug, Clone)]
pub struct ConfigWarning {
    /// Category of the warning.
    pub kind: WarningKind,

    /// Human-readable warning message.
    pub message: String,
}

/// Categories of TOON configuration warnings.
#[derive(Debug, Clone, PartialEq)]
pub enum WarningKind {
    /// A model is defined but not referenced by any agent.
    UnusedModel,

    /// A tool is defined but not referenced by any agent.
    UnusedTool,

    /// An agent is defined but not used in any workflow.
    UnusedAgent,

    /// A workflow is defined but not the default or referenced.
    UnusedWorkflow,

    /// An MCP server is defined but not referenced.
    UnusedMcp,
}

impl std::fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

// ============= Hot Reload Manager =============

/// Manager for dynamic TOON configuration with hot-reload support
///
/// This manager:
/// - Loads all TOON configs at startup
/// - Watches config directories for changes
/// - Atomically swaps config on changes (lock-free reads)
/// - Provides convenient accessor methods
///
/// # Example
///
/// ```rust,ignore
/// let manager = DynamicConfigManager::new(
///     PathBuf::from("config/agents"),
///     PathBuf::from("config/models"),
///     PathBuf::from("config/tools"),
///     PathBuf::from("config/workflows"),
///     PathBuf::from("config/mcps"),
///     true, // hot_reload
/// )?;
///
/// // Get an agent config (lock-free)
/// if let Some(router) = manager.agent("router") {
///     println!("Router uses model: {}", router.model);
/// }
/// ```
pub struct DynamicConfigManager {
    config: Arc<ArcSwap<DynamicConfig>>,
    agents_dir: PathBuf,
    models_dir: PathBuf,
    tools_dir: PathBuf,
    workflows_dir: PathBuf,
    mcps_dir: PathBuf,
    _watcher: Option<RecommendedWatcher>,
}

impl DynamicConfigManager {
    /// Create DynamicConfigManager from AresConfig
    ///
    /// This uses the paths defined in `config.config` (DynamicConfigPaths)
    /// to initialize the manager.
    pub fn from_config(
        config: &crate::utils::toml_config::AresConfig,
    ) -> Result<Self, ToonConfigError> {
        let agents_dir = PathBuf::from(&config.config.agents_dir);
        let models_dir = PathBuf::from(&config.config.models_dir);
        let tools_dir = PathBuf::from(&config.config.tools_dir);
        let workflows_dir = PathBuf::from(&config.config.workflows_dir);
        let mcps_dir = PathBuf::from(&config.config.mcps_dir);

        Self::new(
            agents_dir,
            models_dir,
            tools_dir,
            workflows_dir,
            mcps_dir,
            true, // Enable hot reload by default
        )
    }

    /// Create a new DynamicConfigManager
    ///
    /// # Arguments
    /// * `agents_dir` - Directory containing agent TOON files
    /// * `models_dir` - Directory containing model TOON files
    /// * `tools_dir` - Directory containing tool TOON files
    /// * `workflows_dir` - Directory containing workflow TOON files
    /// * `mcps_dir` - Directory containing MCP TOON files
    /// * `hot_reload` - Whether to watch for file changes
    pub fn new(
        agents_dir: PathBuf,
        models_dir: PathBuf,
        tools_dir: PathBuf,
        workflows_dir: PathBuf,
        mcps_dir: PathBuf,
        hot_reload: bool,
    ) -> Result<Self, ToonConfigError> {
        // Load initial config
        let initial_config = DynamicConfig::load(
            &agents_dir,
            &models_dir,
            &tools_dir,
            &workflows_dir,
            &mcps_dir,
        )?;

        let config = Arc::new(ArcSwap::from_pointee(initial_config));

        // Set up file watcher if hot reload is enabled
        let watcher = if hot_reload {
            Some(Self::setup_watcher(
                config.clone(),
                agents_dir.clone(),
                models_dir.clone(),
                tools_dir.clone(),
                workflows_dir.clone(),
                mcps_dir.clone(),
            )?)
        } else {
            None
        };

        Ok(Self {
            config,
            agents_dir,
            models_dir,
            tools_dir,
            workflows_dir,
            mcps_dir,
            _watcher: watcher,
        })
    }

    /// Set up file watcher for hot-reload
    fn setup_watcher(
        config: Arc<ArcSwap<DynamicConfig>>,
        agents_dir: PathBuf,
        models_dir: PathBuf,
        tools_dir: PathBuf,
        workflows_dir: PathBuf,
        mcps_dir: PathBuf,
    ) -> Result<RecommendedWatcher, ToonConfigError> {
        let agents_dir_clone = agents_dir.clone();
        let models_dir_clone = models_dir.clone();
        let tools_dir_clone = tools_dir.clone();
        let workflows_dir_clone = workflows_dir.clone();
        let mcps_dir_clone = mcps_dir.clone();

        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    // Only reload on create, modify, or remove events
                    if matches!(
                        event.kind,
                        notify::EventKind::Create(_)
                            | notify::EventKind::Modify(_)
                            | notify::EventKind::Remove(_)
                    ) {
                        info!("Config change detected, reloading...");

                        match DynamicConfig::load(
                            &agents_dir_clone,
                            &models_dir_clone,
                            &tools_dir_clone,
                            &workflows_dir_clone,
                            &mcps_dir_clone,
                        ) {
                            Ok(new_config) => {
                                // Validate before swapping
                                match new_config.validate() {
                                    Ok(warnings) => {
                                        for warning in warnings {
                                            warn!("Config warning: {}", warning);
                                        }
                                        config.store(Arc::new(new_config));
                                        info!("Config reloaded successfully");
                                    }
                                    Err(e) => {
                                        error!(
                                            "Config validation failed, keeping old config: {}",
                                            e
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Failed to reload config: {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Watch error: {:?}", e);
                }
            }
        })?;

        // Watch all config directories
        for dir in [
            &agents_dir,
            &models_dir,
            &tools_dir,
            &workflows_dir,
            &mcps_dir,
        ] {
            if dir.exists() {
                watcher.watch(dir, RecursiveMode::Recursive)?;
                debug!("Watching directory: {:?}", dir);
            }
        }

        Ok(watcher)
    }

    /// Get current config snapshot (lock-free)
    pub fn config(&self) -> arc_swap::Guard<Arc<DynamicConfig>> {
        self.config.load()
    }

    /// Get a specific agent config
    pub fn agent(&self, name: &str) -> Option<ToonAgentConfig> {
        self.config.load().get_agent(name).cloned()
    }

    /// Get a specific model config
    pub fn model(&self, name: &str) -> Option<ToonModelConfig> {
        self.config.load().get_model(name).cloned()
    }

    /// Get a specific tool config
    pub fn tool(&self, name: &str) -> Option<ToonToolConfig> {
        self.config.load().get_tool(name).cloned()
    }

    /// Get a specific workflow config
    pub fn workflow(&self, name: &str) -> Option<ToonWorkflowConfig> {
        self.config.load().get_workflow(name).cloned()
    }

    /// Get a specific MCP config
    pub fn mcp(&self, name: &str) -> Option<ToonMcpConfig> {
        self.config.load().get_mcp(name).cloned()
    }

    /// Get all agents
    pub fn agents(&self) -> Vec<ToonAgentConfig> {
        self.config.load().agents.values().cloned().collect()
    }

    /// Get all models
    pub fn models(&self) -> Vec<ToonModelConfig> {
        self.config.load().models.values().cloned().collect()
    }

    /// Get all tools
    pub fn tools(&self) -> Vec<ToonToolConfig> {
        self.config.load().tools.values().cloned().collect()
    }

    /// Get all workflows
    pub fn workflows(&self) -> Vec<ToonWorkflowConfig> {
        self.config.load().workflows.values().cloned().collect()
    }

    /// Get all MCPs
    pub fn mcps(&self) -> Vec<ToonMcpConfig> {
        self.config.load().mcps.values().cloned().collect()
    }

    /// Get all agent names
    pub fn agent_names(&self) -> Vec<String> {
        self.config
            .load()
            .agent_names()
            .into_iter()
            .map(String::from)
            .collect()
    }

    /// Get all model names
    pub fn model_names(&self) -> Vec<String> {
        self.config
            .load()
            .model_names()
            .into_iter()
            .map(String::from)
            .collect()
    }

    /// Get all tool names
    pub fn tool_names(&self) -> Vec<String> {
        self.config
            .load()
            .tool_names()
            .into_iter()
            .map(String::from)
            .collect()
    }

    /// Get all workflow names
    pub fn workflow_names(&self) -> Vec<String> {
        self.config
            .load()
            .workflow_names()
            .into_iter()
            .map(String::from)
            .collect()
    }

    /// Get all MCP names
    pub fn mcp_names(&self) -> Vec<String> {
        self.config
            .load()
            .mcp_names()
            .into_iter()
            .map(String::from)
            .collect()
    }

    /// Manually reload configuration
    pub fn reload(&self) -> Result<Vec<ConfigWarning>, ToonConfigError> {
        let new_config = DynamicConfig::load(
            &self.agents_dir,
            &self.models_dir,
            &self.tools_dir,
            &self.workflows_dir,
            &self.mcps_dir,
        )?;

        let warnings = new_config.validate()?;
        self.config.store(Arc::new(new_config));
        Ok(warnings)
    }
}

// ============= Tests =============

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_agent_config_roundtrip() {
        let agent = ToonAgentConfig::new("test-agent", "fast")
            .with_system_prompt("You are a test agent.")
            .with_tools(vec!["calculator".to_string(), "web_search".to_string()]);

        let toon = agent.to_toon().expect("Failed to encode");
        let decoded = ToonAgentConfig::from_toon(&toon).expect("Failed to decode");

        assert_eq!(agent.name, decoded.name);
        assert_eq!(agent.model, decoded.model);
        assert_eq!(agent.system_prompt, decoded.system_prompt);
        assert_eq!(agent.tools, decoded.tools);
    }

    #[test]
    fn test_model_config_roundtrip() {
        let model = ToonModelConfig::new("fast", "ollama-local", "ministral-3:3b");

        let toon = model.to_toon().expect("Failed to encode");
        let decoded = ToonModelConfig::from_toon(&toon).expect("Failed to decode");

        assert_eq!(model.name, decoded.name);
        assert_eq!(model.provider, decoded.provider);
        assert_eq!(model.model, decoded.model);
        assert_eq!(model.temperature, decoded.temperature);
        assert_eq!(model.max_tokens, decoded.max_tokens);
    }

    #[test]
    fn test_tool_config_roundtrip() {
        let mut tool = ToonToolConfig::new("calculator");
        tool.description = Some("Performs arithmetic operations".to_string());
        tool.timeout_secs = 10;

        let toon = tool.to_toon().expect("Failed to encode");
        let decoded = ToonToolConfig::from_toon(&toon).expect("Failed to decode");

        assert_eq!(tool.name, decoded.name);
        assert_eq!(tool.enabled, decoded.enabled);
        assert_eq!(tool.description, decoded.description);
        assert_eq!(tool.timeout_secs, decoded.timeout_secs);
    }

    #[test]
    fn test_workflow_config_roundtrip() {
        let mut workflow = ToonWorkflowConfig::new("default", "router");
        workflow.fallback_agent = Some("orchestrator".to_string());
        workflow.max_depth = 3;
        workflow.max_iterations = 5;

        let toon = workflow.to_toon().expect("Failed to encode");
        let decoded = ToonWorkflowConfig::from_toon(&toon).expect("Failed to decode");

        assert_eq!(workflow.name, decoded.name);
        assert_eq!(workflow.entry_agent, decoded.entry_agent);
        assert_eq!(workflow.fallback_agent, decoded.fallback_agent);
        assert_eq!(workflow.max_depth, decoded.max_depth);
        assert_eq!(workflow.max_iterations, decoded.max_iterations);
    }

    #[test]
    fn test_mcp_config_roundtrip() {
        let mut mcp = ToonMcpConfig::new("filesystem", "npx");
        mcp.args = vec![
            "-y".to_string(),
            "@modelcontextprotocol/server-filesystem".to_string(),
            "/home".to_string(),
            "/tmp".to_string(),
        ];
        mcp.env
            .insert("NODE_ENV".to_string(), "production".to_string());
        mcp.timeout_secs = 30;

        let toon = mcp.to_toon().expect("Failed to encode");
        let decoded = ToonMcpConfig::from_toon(&toon).expect("Failed to decode");

        assert_eq!(mcp.name, decoded.name);
        assert_eq!(mcp.command, decoded.command);
        assert_eq!(mcp.args, decoded.args);
        assert_eq!(mcp.env, decoded.env);
        assert_eq!(mcp.timeout_secs, decoded.timeout_secs);
    }

    #[test]
    fn test_load_configs_from_dir() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let agents_dir = temp_dir.path().join("agents");
        fs::create_dir_all(&agents_dir).expect("Failed to create agents dir");

        // Create a test agent TOON file
        let agent_content = r#"name: test-agent
model: fast
max_tool_iterations: 5
parallel_tools: false
tools[0]:
system_prompt: Test agent prompt"#;

        fs::write(agents_dir.join("test-agent.toon"), agent_content)
            .expect("Failed to write agent file");

        let agents = load_configs_from_dir::<ToonAgentConfig>(&agents_dir, "agents")
            .expect("Failed to load agents");

        assert_eq!(agents.len(), 1);
        let agent = agents.get("test-agent").expect("Agent not found");
        assert_eq!(agent.name, "test-agent");
        assert_eq!(agent.model, "fast");
        assert_eq!(agent.max_tool_iterations, 5);
    }

    #[test]
    fn test_dynamic_config_validation() {
        let mut config = DynamicConfig::default();

        // Add a model
        config.models.insert(
            "fast".to_string(),
            ToonModelConfig::new("fast", "ollama-local", "ministral-3:3b"),
        );

        // Add a tool
        config
            .tools
            .insert("calculator".to_string(), ToonToolConfig::new("calculator"));

        // Add an agent that uses the model and tool
        let mut agent = ToonAgentConfig::new("router", "fast");
        agent.tools = vec!["calculator".to_string()];
        config.agents.insert("router".to_string(), agent);

        // Add a workflow that uses the agent
        config.workflows.insert(
            "default".to_string(),
            ToonWorkflowConfig::new("default", "router"),
        );

        // Validation should pass
        let warnings = config.validate().expect("Validation failed");
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_dynamic_config_validation_missing_model() {
        let mut config = DynamicConfig::default();

        // Add an agent that references a non-existent model
        let agent = ToonAgentConfig::new("router", "non-existent-model");
        config.agents.insert("router".to_string(), agent);

        // Validation should fail
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown model"));
    }

    #[test]
    fn test_dynamic_config_validation_missing_tool() {
        let mut config = DynamicConfig::default();

        // Add a model
        config.models.insert(
            "fast".to_string(),
            ToonModelConfig::new("fast", "ollama-local", "ministral-3:3b"),
        );

        // Add an agent that references a non-existent tool
        let mut agent = ToonAgentConfig::new("router", "fast");
        agent.tools = vec!["non-existent-tool".to_string()];
        config.agents.insert("router".to_string(), agent);

        // Validation should fail
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown tool"));
    }

    #[test]
    fn test_parse_agent_from_toon_string() {
        let toon = r#"name: router
model: fast
max_tool_iterations: 1
parallel_tools: false
tools[0]:
system_prompt: You are a routing agent."#;

        let agent = ToonAgentConfig::from_toon(toon).expect("Failed to parse");
        assert_eq!(agent.name, "router");
        assert_eq!(agent.model, "fast");
        assert_eq!(agent.max_tool_iterations, 1);
        assert!(!agent.parallel_tools);
        assert!(agent.tools.is_empty());
    }

    #[test]
    fn test_parse_model_from_toon_string() {
        let toon = r#"name: fast
provider: ollama-local
model: ministral-3:3b
temperature: 0.7
max_tokens: 256"#;

        let model = ToonModelConfig::from_toon(toon).expect("Failed to parse");
        assert_eq!(model.name, "fast");
        assert_eq!(model.provider, "ollama-local");
        assert_eq!(model.model, "ministral-3:3b");
        assert!((model.temperature - 0.7).abs() < 0.01);
        assert_eq!(model.max_tokens, 256);
    }
}
