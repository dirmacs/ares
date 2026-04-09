//! Skills module — SKILL.md file discovery and loading via thulp.
//!
//! Provides endpoints for listing and retrieving agent skills from
//! configured directories. Skills are SKILL.md files with YAML frontmatter.
//!
//! # Feature Flag
//!
//! Requires the `skills` feature to be enabled.
//!
//! ```toml
//! [dependencies]
//! ares-server = { version = "0.7", features = ["skills"] }
//! ```

#[cfg(feature = "skills")]
pub mod loader {
    use std::path::PathBuf;
    use thulp_skill_files::{LoadedSkill, SkillLoader, SkillLoaderConfig};

    /// Load all skills from the configured directories.
    ///
    /// Scans project, personal, and plugin directories for SKILL.md files
    /// and returns them with scope-based priority resolution.
    pub fn load_skills(config: &SkillsConfig) -> Vec<LoadedSkill> {
        let loader_config = SkillLoaderConfig {
            project_dir: config.project_dir.clone(),
            personal_dir: config.personal_dir.clone(),
            enterprise_dir: config.enterprise_dir.clone(),
            plugin_dirs: config.plugin_dirs.clone(),
            max_depth: 3,
        };

        let loader = SkillLoader::new(loader_config);
        match loader.load_all() {
            Ok(skills) => {
                tracing::info!(count = skills.len(), "Loaded skills from directories");
                skills
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load skills");
                Vec::new()
            }
        }
    }

    /// List skill names and descriptions (lightweight, no full content).
    pub fn list_skills(config: &SkillsConfig) -> Vec<SkillSummary> {
        load_skills(config)
            .into_iter()
            .map(|s| {
                let fqn = s.qualified_name();
                SkillSummary {
                    name: fqn,
                    description: s.file.frontmatter.description.clone().unwrap_or_default(),
                    scope: s.scope.to_string(),
                    path: s.file.path.to_string_lossy().to_string(),
                }
            })
            .collect()
    }

    /// Get a single skill by name.
    pub fn get_skill(config: &SkillsConfig, name: &str) -> Option<LoadedSkill> {
        load_skills(config).into_iter().find(|s| s.qualified_name() == name)
    }

    /// Skills configuration — where to look for SKILL.md files.
    #[derive(Debug, Clone, Default, serde::Deserialize)]
    pub struct SkillsConfig {
        /// Project skills directory (e.g., ./.claude/skills/).
        pub project_dir: Option<PathBuf>,
        /// Personal skills directory (e.g., ~/.claude/skills/).
        pub personal_dir: Option<PathBuf>,
        /// Enterprise skills directory.
        pub enterprise_dir: Option<PathBuf>,
        /// Plugin directories to scan.
        #[serde(default)]
        pub plugin_dirs: Vec<PathBuf>,
    }

    /// Lightweight skill summary for list endpoints.
    #[derive(Debug, Clone, serde::Serialize)]
    pub struct SkillSummary {
        pub name: String,
        pub description: String,
        pub scope: String,
        pub path: String,
    }
}

#[cfg(feature = "skills")]
pub use loader::*;
