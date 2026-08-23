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
#[cfg(unix)]
type ConfigFsNotify = notify::INotifyWatcher;
#[cfg(windows)]
type ConfigFsNotify = notify::ReadDirectoryChangesWatcher;
#[cfg(not(any(unix, windows)))]
type ConfigFsNotify = notify::PollWatcher;
use notify::{Event, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::any::TypeId;
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

    /// Semantic version of this config (e.g. "1.0.0")
    /// Increment on any behavior-changing edit. Stored in agent_config_versions on every change.
    #[serde(default = "default_version")]
    pub version: String,

    /// Reference to a model name defined in `config/models/`
    pub model: String,

    /// System prompt defining agent behavior
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// List of tool names this agent can use (defined in `config/tools/`)
    #[serde(default)]
    pub tools: Vec<String>,

    /// Optional whitelist of tool names this agent is allowed to use.
    /// If absent, all tools are permitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,

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

fn default_version() -> String {
    "0.1.0".to_string()
}

fn default_max_tool_iterations() -> usize {
    10
}

impl ToonAgentConfig {
    /// Create a new agent config with required fields
    pub fn new(name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: default_version(),
            model: model.into(),
            system_prompt: None,
            tools: Vec::new(),
            allowed_tools: None,
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

    /// Command to run the MCP server (e.g., "npx", "python"). Optional for HTTP transport.
    #[serde(default)]
    pub command: Option<String>,

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
            command: Some(command.into()),
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

    match decode_default(&content) {
        Ok(config) => Ok(config),
        Err(toon_error) => toml::from_str(&content).map_err(|toml_error| {
            ToonConfigError::Parse(format!(
                "Failed to parse {:?} as TOON ({}) or TOML ({})",
                path, toon_error, toml_error
            ))
        }),
    }
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
    _watcher: Option<ConfigFsNotify>,
    /// Shared sender for version change events. Populated via `set_version_tx`.
    /// The watcher closure holds a clone of this Arc, so setting it after construction
    /// is visible inside the watcher.
    version_tx:
        Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<ToonAgentConfig>>>>>,
}

impl DynamicConfigManager {
    /// Create DynamicConfigManager from AresConfig
    ///
    /// This uses the paths defined in `config.config` (DynamicConfigPaths)
    /// to initialize the manager.
    pub fn from_config(
        config: &crate::overlay::AresConfig,
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
            false, // Overlay owns the single watch_many_with; no second notify watcher
        )
    }

    /// Reload TOON files from the configured directories (Overlay watch callback).
    pub fn reload(&self) -> Result<Vec<ConfigWarning>, ToonConfigError> {
        let new_config = DynamicConfig::load(
            &self.agents_dir,
            &self.models_dir,
            &self.tools_dir,
            &self.workflows_dir,
            &self.mcps_dir,
        )?;
        match new_config.validate() {
            Ok(warnings) => {
                for warning in &warnings {
                    warn!("Config warning: {}", warning);
                }
                let agents: Vec<ToonAgentConfig> = new_config.agents.values().cloned().collect();
                if let Ok(guard) = self.version_tx.lock() {
                    if let Some(tx) = guard.as_ref() {
                        let _ = tx.send(agents);
                    }
                }
                self.config.store(Arc::new(new_config));
                info!("Config reloaded successfully");
                Ok(warnings)
            }
            Err(e) => {
                error!("Config validation failed, keeping old config: {}", e);
                Err(e)
            }
        }
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

        // Shared version channel — populated after construction via set_version_tx
        let version_tx: Arc<
            std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<ToonAgentConfig>>>>,
        > = Arc::new(std::sync::Mutex::new(None));

        // Overlay::watch_cordis owns the single watch_many_with stack.
        // Do not start a second notify watcher from DynamicConfigManager.
        let _ = hot_reload;
        let watcher = None;

        Ok(Self {
            config,
            agents_dir,
            models_dir,
            tools_dir,
            workflows_dir,
            mcps_dir,
            _watcher: watcher,
            version_tx,
        })
    }

    /// Attach a version tracking sender. After this call, every hot-reload emits the
    /// updated agent list to this channel for a background task to persist to DB.
    pub fn set_version_tx(&self, tx: tokio::sync::mpsc::UnboundedSender<Vec<ToonAgentConfig>>) {
        if let Ok(mut guard) = self.version_tx.lock() {
            *guard = Some(tx);
        }
    }

    /// Set up file watcher for hot-reload (unused: Overlay owns watch_many_with).
    #[allow(dead_code)]
    fn setup_watcher(
        config: Arc<ArcSwap<DynamicConfig>>,
        agents_dir: PathBuf,
        models_dir: PathBuf,
        tools_dir: PathBuf,
        workflows_dir: PathBuf,
        mcps_dir: PathBuf,
        version_tx: Arc<
            std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<ToonAgentConfig>>>>,
        >,
    ) -> Result<ConfigFsNotify, ToonConfigError> {
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
                                        // Emit version change event before swapping
                                        let agents: Vec<ToonAgentConfig> =
                                            new_config.agents.values().cloned().collect();
                                        if let Ok(guard) = version_tx.lock() {
                                            if let Some(tx) = guard.as_ref() {
                                                let _ = tx.send(agents);
                                            }
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

    /// Hot-swap a single agent config in the in-memory cache (used for rollback).
    /// Does not write to disk — disk files are the canonical source on next restart.
    pub fn upsert_agent(&self, agent: ToonAgentConfig) {
        let current = self.config.load();
        let mut new_agents = current.agents.clone();
        new_agents.insert(agent.name.clone(), agent);
        let new_config = DynamicConfig {
            agents: new_agents,
            models: current.models.clone(),
            tools: current.tools.clone(),
            workflows: current.workflows.clone(),
            mcps: current.mcps.clone(),
        };
        self.config.store(Arc::new(new_config));
    }
}

impl cordis::Service for DynamicConfigManager {
    fn name(&self) -> &'static str { "dynamic_config_manager" }
    fn init(&self, _ctx: &std::sync::Arc<cordis::Context>) -> cordis::ServiceInitFuture<'_> {
        // Overlay::watch_cordis owns the single watch_many_with for TOON dirs.
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool { true }
}

/// Notify Tools and Execute TypeIds after a TOON reload.
///
/// Overlay's `watch_many_with` callback calls this instead of starting a
/// second notify watcher on [`DynamicConfigManager`].
pub(crate) fn notify_tools_and_execute(ctx: &Arc<cordis::Context>) {
    if let Some(reflect) = ctx.get::<cordis::ReflectService>() {
        reflect.notify(TypeId::of::<ares_tools::Tools>());
        reflect.notify(TypeId::of::<ares_agent::Execute>());
    }
}

fn json_extra_to_toml(extra: &HashMap<String, serde_json::Value>) -> HashMap<String, toml::Value> {
    extra
        .iter()
        .filter_map(|(k, v)| {
            serde_json::from_value::<toml::Value>(v.clone())
                .ok()
                .map(|tv| (k.clone(), tv))
        })
        .collect()
}

fn toon_to_agent_config(t: &ToonAgentConfig) -> ares_agent::AgentConfig {
    ares_agent::AgentConfig {
        model: t.model.clone(),
        system_prompt: t.system_prompt.clone(),
        tools: t.tools.clone(),
        allowed_tools: t.allowed_tools.clone(),
        max_tool_iterations: t.max_tool_iterations,
        parallel_tools: t.parallel_tools,
        extra: json_extra_to_toml(&t.extra),
    }
}

impl ares_agent::ToonAgents for DynamicConfigManager {
    fn get(&self, name: &str) -> Option<ares_agent::AgentConfig> {
        self.agent(name).as_ref().map(toon_to_agent_config)
    }
    fn names(&self) -> Vec<String> {
        self.agent_names()
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
    fn test_load_toml_shaped_toon_config_from_dir() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mcps_dir = temp_dir.path().join("mcps");
        fs::create_dir_all(&mcps_dir).expect("Failed to create mcps dir");

        let mcp_content = r#"name = "eruka"
enabled = true
endpoint = "https://eruka.dirmacs.com/mcp"
transport = "http"
timeout_secs = 30
"#;

        fs::write(mcps_dir.join("eruka.toon"), mcp_content).expect("Failed to write mcp file");

        let mcps =
            load_configs_from_dir::<ToonMcpConfig>(&mcps_dir, "mcps").expect("Failed to load mcps");

        let mcp = mcps.get("eruka").expect("MCP not found");
        assert_eq!(mcp.name, "eruka");
        assert!(mcp.enabled);
        assert_eq!(mcp.timeout_secs, 30);
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

        let err = config.validate().expect_err("expected validation error");
        assert_eq!(
            err.to_string(),
            "Validation error: Agent 'router' references unknown model 'non-existent-model'"
        );
        match err {
            ToonConfigError::Validation(msg) => {
                assert_eq!(
                    msg,
                    "Agent 'router' references unknown model 'non-existent-model'"
                );
            }
            other => panic!("expected Validation error, got {other:?}"),
        }
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

        let err = config.validate().expect_err("expected validation error");
        match err {
            ToonConfigError::Validation(msg) => {
                assert_eq!(
                    msg,
                    "Agent 'router' references unknown tool 'non-existent-tool'"
                );
            }
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    #[test]
    fn test_dynamic_config_validation_missing_entry_agent() {
        let mut config = DynamicConfig::default();
        config.workflows.insert(
            "default".to_string(),
            ToonWorkflowConfig::new("default", "missing-agent"),
        );

        let err = config.validate().expect_err("expected validation error");
        assert_eq!(
            err.to_string(),
            "Validation error: Workflow 'default' references unknown entry agent 'missing-agent'"
        );
    }

    #[test]
    fn test_dynamic_config_validation_missing_fallback_agent() {
        let mut config = DynamicConfig::default();
        config
            .agents
            .insert("router".to_string(), ToonAgentConfig::new("router", "fast"));
        config.models.insert(
            "fast".to_string(),
            ToonModelConfig::new("fast", "ollama", "llama3"),
        );

        let mut workflow = ToonWorkflowConfig::new("default", "router");
        workflow.fallback_agent = Some("missing-fallback".to_string());
        config.workflows.insert("default".to_string(), workflow);

        let err = config.validate().expect_err("expected validation error");
        assert_eq!(
            err.to_string(),
            "Validation error: Workflow 'default' references unknown fallback agent 'missing-fallback'"
        );
    }

    #[test]
    fn test_dynamic_config_validation_unused_model_and_tool_warnings() {
        let mut config = DynamicConfig::default();
        config.models.insert(
            "fast".to_string(),
            ToonModelConfig::new("fast", "ollama", "llama3"),
        );
        config.models.insert(
            "slow".to_string(),
            ToonModelConfig::new("slow", "ollama", "llama3:70b"),
        );
        config
            .tools
            .insert("calc".to_string(), ToonToolConfig::new("calc"));
        config
            .tools
            .insert("search".to_string(), ToonToolConfig::new("search"));

        let mut agent = ToonAgentConfig::new("router", "fast");
        agent.tools = vec!["calc".to_string()];
        config.agents.insert("router".to_string(), agent);

        let warnings = config.validate().expect("validation should succeed");
        assert_eq!(warnings.len(), 2);

        let unused_model = warnings
            .iter()
            .find(|w| w.kind == WarningKind::UnusedModel)
            .expect("unused model warning");
        assert_eq!(
            unused_model.message,
            "Model 'slow' is not used by any agent"
        );
        assert_eq!(unused_model.to_string(), unused_model.message);

        let unused_tool = warnings
            .iter()
            .find(|w| w.kind == WarningKind::UnusedTool)
            .expect("unused tool warning");
        assert_eq!(
            unused_tool.message,
            "Tool 'search' is not used by any agent"
        );
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
    #[test]
    fn test_toon_agent_config_defaults() {
        let agent = ToonAgentConfig::new("router", "fast");
        assert_eq!(agent.version, "0.1.0");
        assert_eq!(agent.max_tool_iterations, 10);
        assert!(!agent.parallel_tools);
        assert!(agent.tools.is_empty());
        assert!(agent.system_prompt.is_none());
    }

    #[test]
    fn test_toon_model_config_defaults() {
        let model = ToonModelConfig::new("fast", "ollama", "llama3");
        assert!((model.temperature - 0.7).abs() < 0.01);
        assert_eq!(model.max_tokens, 512);
    }

    #[test]
    fn test_toon_tool_config_defaults() {
        let tool = ToonToolConfig::new("calc");
        assert!(tool.enabled);
        assert_eq!(tool.timeout_secs, 30);
    }

    #[test]
    fn test_toon_workflow_config_defaults() {
        let wf = ToonWorkflowConfig::new("main", "router");
        assert_eq!(wf.max_depth, 3);
        assert_eq!(wf.max_iterations, 5);
        assert!(wf.fallback_agent.is_none());
    }

    #[test]
    fn test_parse_tool_from_toon_string() {
        let toon = r#"name: calculator
enabled: true
timeout_secs: 15
description: Performs arithmetic"#;

        let tool = ToonToolConfig::from_toon(toon).expect("Failed to parse");
        assert_eq!(tool.name, "calculator");
        assert!(tool.enabled);
        assert_eq!(tool.timeout_secs, 15);
        assert_eq!(
            tool.description.as_deref(),
            Some("Performs arithmetic")
        );
    }

    #[test]
    fn test_parse_workflow_from_toon_string() {
        let toon = r#"name: default
entry_agent: router
fallback_agent: orchestrator
max_depth: 2
max_iterations: 4
parallel_subagents: true"#;

        let workflow = ToonWorkflowConfig::from_toon(toon).expect("Failed to parse");
        assert_eq!(workflow.name, "default");
        assert_eq!(workflow.entry_agent, "router");
        assert_eq!(workflow.fallback_agent.as_deref(), Some("orchestrator"));
        assert_eq!(workflow.max_depth, 2);
        assert_eq!(workflow.max_iterations, 4);
        assert!(workflow.parallel_subagents);
    }

    #[test]
    fn test_parse_agent_applies_serde_defaults_from_toon() {
        let toon = r#"name: minimal
model: fast"#;

        let agent = ToonAgentConfig::from_toon(toon).expect("Failed to parse");
        assert_eq!(agent.version, "0.1.0");
        assert_eq!(agent.max_tool_iterations, 10);
        assert!(!agent.parallel_tools);
        assert!(agent.tools.is_empty());
    }

    #[test]
    fn test_parse_invalid_toon_returns_parse_error() {
        let err = ToonAgentConfig::from_toon("name: [unclosed").expect_err("expected parse error");
        assert!(err.to_string().starts_with("TOON parse error: "));
        match err {
            ToonConfigError::Parse(msg) => assert!(!msg.is_empty()),
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn test_load_toon_file_invalid_content() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let path = temp_dir.path().join("broken.toon");
        fs::write(&path, "name: [unclosed").expect("Failed to write broken toon file");

        let err = load_toon_file::<ToonAgentConfig>(&path).expect_err("expected parse error");
        let msg = err.to_string();
        assert!(msg.contains("Failed to parse"), "{msg}");
        assert!(msg.contains("broken.toon"), "{msg}");
        assert!(msg.contains("TOON"), "{msg}");
        assert!(msg.contains("TOML"), "{msg}");
    }

    #[test]
    fn test_load_configs_from_dir_missing_directory_returns_empty() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let missing = temp_dir.path().join("does-not-exist");

        let agents =
            load_configs_from_dir::<ToonAgentConfig>(&missing, "agents").expect("should succeed");
        assert!(agents.is_empty());
    }

    #[test]
    fn test_load_configs_from_dir_skips_non_toon_files() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let agents_dir = temp_dir.path().join("agents");
        fs::create_dir_all(&agents_dir).expect("Failed to create agents dir");

        fs::write(agents_dir.join("notes.txt"), "ignore me").expect("Failed to write txt file");
        fs::write(
            agents_dir.join("valid.toon"),
            "name: router
model: fast
",
        )
        .expect("Failed to write toon file");

        let agents = load_configs_from_dir::<ToonAgentConfig>(&agents_dir, "agents")
            .expect("Failed to load agents");
        assert_eq!(agents.len(), 1);
        assert!(agents.contains_key("router"));
    }

    #[test]
    fn test_toon_mcp_config_defaults() {
        let mcp = ToonMcpConfig::new("filesystem", "npx");
        assert!(mcp.enabled);
        assert_eq!(mcp.timeout_secs, 30);
        assert_eq!(mcp.command.as_deref(), Some("npx"));
        assert!(mcp.args.is_empty());
        assert!(mcp.env.is_empty());
    }

    #[test]
    fn test_dynamic_config_paths_resolve_under_cwd() {
        let paths = crate::overlay::DynamicConfigPaths::default();
        assert!(paths.agents_dir.is_relative());
        assert!(paths.hot_reload);
    }


    #[test]
    fn test_dynamic_config_load_from_directories() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();

        let agents_dir = root.join("agents");
        let models_dir = root.join("models");
        let tools_dir = root.join("tools");
        let workflows_dir = root.join("workflows");
        let mcps_dir = root.join("mcps");
        for dir in [&agents_dir, &models_dir, &tools_dir, &workflows_dir, &mcps_dir] {
            fs::create_dir_all(dir).expect("Failed to create config dir");
        }

        fs::write(
            agents_dir.join("router.toon"),
            "name: router\nmodel: fast\ntools[1]: calc\n",
        )
        .expect("Failed to write agent");
        fs::write(
            models_dir.join("fast.toon"),
            "name: fast\nprovider: ollama\nmodel: llama3\n",
        )
        .expect("Failed to write model");
        fs::write(tools_dir.join("calc.toon"), "name: calc\n").expect("Failed to write tool");
        fs::write(
            workflows_dir.join("default.toon"),
            "name: default\nentry_agent: router\n",
        )
        .expect("Failed to write workflow");
        fs::write(mcps_dir.join("fs.toon"), "name: fs\ncommand: npx\n")
            .expect("Failed to write mcp");

        let config = DynamicConfig::load(
            &agents_dir,
            &models_dir,
            &tools_dir,
            &workflows_dir,
            &mcps_dir,
        )
        .expect("Failed to load dynamic config");

        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.models.len(), 1);
        assert_eq!(config.tools.len(), 1);
        assert_eq!(config.workflows.len(), 1);
        assert_eq!(config.mcps.len(), 1);
        assert!(config.validate().expect("validation failed").is_empty());
    }

    #[test]
    fn test_dynamic_config_accessors_and_name_lists() {
        let mut config = DynamicConfig::default();
        config
            .agents
            .insert("router".to_string(), ToonAgentConfig::new("router", "fast"));
        config.models.insert(
            "fast".to_string(),
            ToonModelConfig::new("fast", "ollama", "llama3"),
        );
        config
            .tools
            .insert("calc".to_string(), ToonToolConfig::new("calc"));
        config.workflows.insert(
            "default".to_string(),
            ToonWorkflowConfig::new("default", "router"),
        );
        config
            .mcps
            .insert("fs".to_string(), ToonMcpConfig::new("fs", "npx"));

        assert_eq!(config.get_agent("router").unwrap().name, "router");
        assert_eq!(config.get_model("fast").unwrap().provider, "ollama");
        assert_eq!(config.get_tool("calc").unwrap().name, "calc");
        assert_eq!(config.get_workflow("default").unwrap().entry_agent, "router");
        assert_eq!(config.get_mcp("fs").unwrap().name, "fs");
        assert!(config.get_agent("missing").is_none());

        assert_eq!(config.agent_names(), vec!["router"]);
        assert_eq!(config.model_names(), vec!["fast"]);
        assert_eq!(config.tool_names(), vec!["calc"]);
        assert_eq!(config.workflow_names(), vec!["default"]);
        assert_eq!(config.mcp_names(), vec!["fs"]);
    }

    #[test]
    fn test_dynamic_config_manager_without_hot_reload() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();

        let agents_dir = root.join("agents");
        let models_dir = root.join("models");
        let tools_dir = root.join("tools");
        let workflows_dir = root.join("workflows");
        let mcps_dir = root.join("mcps");
        fs::create_dir_all(&agents_dir).expect("Failed to create agents dir");
        fs::create_dir_all(&models_dir).expect("Failed to create models dir");
        fs::create_dir_all(&tools_dir).expect("Failed to create tools dir");
        fs::create_dir_all(&workflows_dir).expect("Failed to create workflows dir");
        fs::create_dir_all(&mcps_dir).expect("Failed to create mcps dir");

        fs::write(
            agents_dir.join("router.toon"),
            "name: router\nmodel: fast\n",
        )
        .expect("Failed to write agent");
        fs::write(
            models_dir.join("fast.toon"),
            "name: fast\nprovider: ollama\nmodel: llama3\n",
        )
        .expect("Failed to write model");

        let manager = DynamicConfigManager::new(
            agents_dir,
            models_dir,
            tools_dir,
            workflows_dir,
            mcps_dir,
            false,
        )
        .expect("Failed to create manager");

        let agent = manager.agent("router").expect("router agent missing");
        assert_eq!(agent.model, "fast");
        assert_eq!(manager.agent_names(), vec!["router"]);
        assert_eq!(manager.model_names(), vec!["fast"]);
        assert!(manager.tool_names().is_empty());
        assert!(manager.workflow_names().is_empty());
        assert!(manager.mcps().is_empty());

        let warnings = manager.reload().expect("reload failed");
        assert!(warnings.is_empty());
    }

    #[test]
    fn dynamic_config_manager_readable_via_cordis() {
        use cordis::Service;
        let dir = TempDir::new().unwrap();
        let manager = DynamicConfigManager::new(
            dir.path().join("agents"),
            dir.path().join("models"),
            dir.path().join("tools"),
            dir.path().join("workflows"),
            dir.path().join("mcps"),
            false,
        )
        .expect("empty dynamic config");
        let ctx = std::sync::Arc::new(cordis::Context::new_root());
        ctx.provide(manager);
        let got = ctx.get::<DynamicConfigManager>().expect("provided");
        assert_eq!(got.name(), "dynamic_config_manager");
        assert!(got.check());
    }

    #[test]
    fn toon_changes_notify_tools_and_execute_type_ids() {
        let ctx = cordis::Context::new_root();
        let reflect = ctx.provide(cordis::ReflectService::new());
        let mut tools_rx = reflect.ensure_notifier(TypeId::of::<ares_tools::Tools>());
        let mut execute_rx = reflect.ensure_notifier(TypeId::of::<ares_agent::Execute>());
        let _ = tools_rx.borrow_and_update();
        let _ = execute_rx.borrow_and_update();

        super::notify_tools_and_execute(&ctx);

        assert!(
            tools_rx.has_changed().expect("tools watch"),
            "TOON notify must signal TypeId::of::<Tools>()"
        );
        assert!(
            execute_rx.has_changed().expect("execute watch"),
            "TOON notify must signal TypeId::of::<Execute>()"
        );
    }

}
