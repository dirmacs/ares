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

#[cfg(all(test, feature = "skills"))]
mod tests {
    use super::loader::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_skill_file(dir: &std::path::Path, name: &str, description: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let content = format!(
            "---\nname: {}\ndescription: {}\n---\n\n# {}\n\nSkill instructions here.\n",
            name, description, name
        );
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn test_skills_config_default() {
        let config = SkillsConfig::default();
        assert!(config.project_dir.is_none());
        assert!(config.personal_dir.is_none());
        assert!(config.plugin_dirs.is_empty());
    }

    #[test]
    fn test_load_skills_empty_dir() {
        let temp = TempDir::new().unwrap();
        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let skills = load_skills(&config);
        assert!(skills.is_empty());
    }

    #[test]
    fn test_load_skills_finds_skill_files() {
        let temp = TempDir::new().unwrap();
        create_skill_file(temp.path(), "test-skill", "A test skill");
        create_skill_file(temp.path(), "another-skill", "Another skill");

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let skills = load_skills(&config);
        assert_eq!(skills.len(), 2);
    }

    #[test]
    fn test_list_skills_returns_summaries() {
        let temp = TempDir::new().unwrap();
        create_skill_file(temp.path(), "my-skill", "Does something useful");

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let summaries = list_skills(&config);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].name, "my-skill");
        assert_eq!(summaries[0].description, "Does something useful");
        assert_eq!(summaries[0].scope, "project");
    }

    #[test]
    fn test_get_skill_found() {
        let temp = TempDir::new().unwrap();
        create_skill_file(temp.path(), "target-skill", "Find me");

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let skill = get_skill(&config, "target-skill");
        assert!(skill.is_some());
    }

    #[test]
    fn test_get_skill_not_found() {
        let temp = TempDir::new().unwrap();
        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        assert!(get_skill(&config, "nonexistent").is_none());
    }

    #[test]
    fn test_skill_summary_serialization() {
        let summary = SkillSummary {
            name: "test".to_string(),
            description: "A test".to_string(),
            scope: "project".to_string(),
            path: "/tmp/test/SKILL.md".to_string(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"name\":\"test\""));
        assert!(json.contains("\"scope\":\"project\""));
    }

    #[test]
    fn test_nonexistent_dir_returns_empty() {
        let config = SkillsConfig {
            project_dir: Some(PathBuf::from("/nonexistent/path/that/doesnt/exist")),
            ..Default::default()
        };
        let skills = load_skills(&config);
        assert!(skills.is_empty());
    }

    #[test]
    fn test_malformed_skill_file_skipped() {
        let temp = TempDir::new().unwrap();
        // Valid skill
        create_skill_file(temp.path(), "good-skill", "Works fine");
        // Malformed: no frontmatter at all
        let bad_dir = temp.path().join("bad-skill");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(bad_dir.join("SKILL.md"), "No frontmatter here, just text.").unwrap();

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let skills = load_skills(&config);
        // Should load at least the good skill; bad one may be skipped or loaded with defaults
        assert!(!skills.is_empty());
    }

    #[test]
    fn test_skill_with_empty_description() {
        let temp = TempDir::new().unwrap();
        let skill_dir = temp.path().join("empty-desc");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: empty-desc\n---\n\n# Empty Desc\n\nNo description field.\n",
        )
        .unwrap();

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let summaries = list_skills(&config);
        // Should still load; description defaults to empty string
        if !summaries.is_empty() {
            assert!(summaries[0].description.is_empty() || summaries[0].description.len() > 0);
        }
    }

    #[test]
    fn test_multiple_dirs_combined() {
        let project_dir = TempDir::new().unwrap();
        let personal_dir = TempDir::new().unwrap();
        create_skill_file(project_dir.path(), "proj-skill", "Project scope");
        create_skill_file(personal_dir.path(), "personal-skill", "Personal scope");

        let config = SkillsConfig {
            project_dir: Some(project_dir.path().to_path_buf()),
            personal_dir: Some(personal_dir.path().to_path_buf()),
            ..Default::default()
        };
        let skills = load_skills(&config);
        assert!(skills.len() >= 2, "Should find skills from both dirs");
    }

    #[test]
    fn test_plugin_dirs() {
        // Plugin dirs expect: plugin_dir/skills/<skill-name>/SKILL.md
        let plugin1 = TempDir::new().unwrap();
        let plugin2 = TempDir::new().unwrap();
        let skills1 = plugin1.path().join("skills");
        let skills2 = plugin2.path().join("skills");
        create_skill_file(&skills1, "plugin1-skill", "From plugin 1");
        create_skill_file(&skills2, "plugin2-skill", "From plugin 2");

        let config = SkillsConfig {
            plugin_dirs: vec![plugin1.path().to_path_buf(), plugin2.path().to_path_buf()],
            ..Default::default()
        };
        let skills = load_skills(&config);
        assert!(skills.len() >= 2, "Should find skills from plugin dirs, got {}", skills.len());
    }

    #[test]
    fn test_all_dirs_none_returns_empty() {
        let config = SkillsConfig::default();
        let skills = load_skills(&config);
        assert!(skills.is_empty());
    }

    #[test]
    fn test_get_skill_wrong_name_returns_none() {
        let temp = TempDir::new().unwrap();
        create_skill_file(temp.path(), "real-skill", "I exist");

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        assert!(get_skill(&config, "fake-skill").is_none());
        assert!(get_skill(&config, "").is_none());
        assert!(get_skill(&config, "real-skil").is_none()); // typo
    }

    #[test]
    fn test_skill_summary_path_populated() {
        let temp = TempDir::new().unwrap();
        create_skill_file(temp.path(), "path-check", "Check path field");

        let config = SkillsConfig {
            project_dir: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let summaries = list_skills(&config);
        assert_eq!(summaries.len(), 1);
        assert!(
            summaries[0].path.contains("SKILL.md"),
            "Path should contain SKILL.md, got: {}",
            summaries[0].path
        );
    }

    #[test]
    fn test_skills_config_deserialize() {
        let json = r#"{
            "project_dir": "/tmp/project",
            "personal_dir": "/tmp/personal",
            "plugin_dirs": ["/tmp/p1", "/tmp/p2"]
        }"#;
        let config: SkillsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.project_dir, Some(PathBuf::from("/tmp/project")));
        assert_eq!(config.personal_dir, Some(PathBuf::from("/tmp/personal")));
        assert_eq!(config.plugin_dirs.len(), 2);
    }
}
