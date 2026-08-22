use serde::{Deserialize, Serialize};

// ============= Workflow Configuration =============

/// Workflow configuration defining agent orchestration patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowConfig {
    /// Entry point agent that receives initial requests.
    pub entry_agent: String,

    /// Fallback agent if routing fails or no match is found.
    pub fallback_agent: Option<String>,

    /// Maximum depth for recursive/nested workflows (default: 3).
    #[serde(default = "default_max_depth")]
    pub max_depth: u8,

    /// Maximum iterations for research/iterative workflows (default: 5).
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u8,

    /// Whether to execute sub-agent calls in parallel.
    #[serde(default)]
    pub parallel_subagents: bool,
}

fn default_max_depth() -> u8 {
    3
}

fn default_max_iterations() -> u8 {
    5
}

// ============= Skills Configuration =============
// Skills directory configuration for SKILL.md discovery.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillsTomlConfig {
    /// Project skills directory (e.g., ./.claude/skills/).
    pub project_dir: Option<std::path::PathBuf>,
    /// Personal skills directory (e.g., ~/.claude/skills/).
    pub personal_dir: Option<std::path::PathBuf>,
    /// Plugin directories to scan for skills.
    pub plugin_dirs: Option<Vec<std::path::PathBuf>>,
}
