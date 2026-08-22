//! Skills HTTP handlers — list and retrieve SKILL.md files via API.
//!
//! Requires the `skills` feature flag.

use std::sync::Arc;
use cordis::Context;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Serialize;

#[derive(Serialize)]
pub struct SkillResponse {
    pub name: String,
    pub description: String,
    pub scope: String,
    pub path: String,
}

#[derive(Serialize)]
pub struct SkillDetailResponse {
    pub name: String,
    pub description: String,
    pub scope: String,
    pub path: String,
    pub content: String,
}

#[derive(Serialize)]
pub struct SkillsListResponse {
    pub skills: Vec<SkillResponse>,
    pub count: usize,
}

/// GET /api/skills — list all discovered skills with scope-based priority.
pub async fn list_skills(
    State(ctx): State<Arc<Context>>,
) -> Json<SkillsListResponse> {
    let config = skills_config_from_state(&ctx);
    let summaries = ares_agent::skills::list_skills(&config);
    let count = summaries.len();
    let skills = summaries
        .into_iter()
        .map(|s| SkillResponse {
            name: s.name,
            description: s.description,
            scope: s.scope,
            path: s.path,
        })
        .collect();
    Json(SkillsListResponse { skills, count })
}

/// GET /api/skills/{name} — get a single skill by qualified name.
pub async fn get_skill(
    State(ctx): State<Arc<Context>>,
    Path(name): Path<String>,
) -> Result<Json<SkillDetailResponse>, StatusCode> {
    let config = skills_config_from_state(&ctx);
    match ares_agent::skills::get_skill(&config, &name) {
        Some(skill) => {
            let fqn = skill.qualified_name();
            Ok(Json(SkillDetailResponse {
                name: fqn,
                description: skill.file.frontmatter.description.clone().unwrap_or_default(),
                scope: skill.scope.to_string(),
                path: skill.file.path.to_string_lossy().to_string(),
                content: skill.file.content.clone(),
            }))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// Build SkillsConfig from Arc<Context> config manager.
fn skills_config_from_state(ctx: &Arc<Context>) -> ares_agent::skills::SkillsConfig {
    let config = ctx.get::<crate::overlay::AresConfigManager>().expect("not provided").config();
    match &config.skills {
        Some(skills_toml) => ares_agent::skills::SkillsConfig {
            project_dir: skills_toml.project_dir.clone(),
            personal_dir: skills_toml.personal_dir.clone(),
            enterprise_dir: None,
            plugin_dirs: skills_toml.plugin_dirs.clone().unwrap_or_default(),
        },
        None => ares_agent::skills::SkillsConfig::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_response_serialization() {
        let resp = SkillResponse {
            name: "test-skill".to_string(),
            description: "A test".to_string(),
            scope: "project".to_string(),
            path: "/tmp/test/SKILL.md".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("test-skill"));
        assert!(json.contains("project"));
    }

    #[test]
    fn test_skill_detail_response_serialization() {
        let resp = SkillDetailResponse {
            name: "my-skill".to_string(),
            description: "Does things".to_string(),
            scope: "personal".to_string(),
            path: "/home/user/.claude/skills/my-skill/SKILL.md".to_string(),
            content: "# My Skill\n\nInstructions here.".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("my-skill"));
        assert!(json.contains("Instructions here"));
    }

    #[test]
    fn test_skills_list_response_serialization() {
        let resp = SkillsListResponse {
            skills: vec![],
            count: 0,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"count\":0"));
        assert!(json.contains("\"skills\":[]"));
    }

    #[test]
    fn test_skills_list_with_items() {
        let resp = SkillsListResponse {
            skills: vec![
                SkillResponse {
                    name: "a".to_string(),
                    description: "first".to_string(),
                    scope: "project".to_string(),
                    path: "/a/SKILL.md".to_string(),
                },
                SkillResponse {
                    name: "b".to_string(),
                    description: "second".to_string(),
                    scope: "personal".to_string(),
                    path: "/b/SKILL.md".to_string(),
                },
            ],
            count: 2,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"count\":2"));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["skills"].as_array().unwrap().len(), 2);
    }
}
