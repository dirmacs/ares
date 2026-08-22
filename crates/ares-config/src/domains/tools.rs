use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============= Tool Configuration =============

/// Tool configuration for built-in or custom tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConfig {
    /// Whether the tool is enabled (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Optional human-readable description of the tool.
    #[serde(default)]
    pub description: Option<String>,

    /// Timeout in seconds for tool execution (default: 30).
    #[serde(default = "default_tool_timeout")]
    pub timeout_secs: u64,

    /// Additional tool-specific configuration passed through.
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
