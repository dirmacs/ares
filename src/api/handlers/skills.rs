//! Skills HTTP handlers — list and retrieve SKILL.md files via API.
//!
//! Requires the `skills` feature flag.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Serialize;

use crate::AppState;

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
    State(state): State<AppState>,
) -> Json<SkillsListResponse> {
    let config = skills_config_from_state(&state);
    let summaries = crate::skills::list_skills(&config);
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
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<SkillDetailResponse>, StatusCode> {
    let config = skills_config_from_state(&state);
    match crate::skills::get_skill(&config, &name) {
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

/// Build SkillsConfig from AppState config manager.
fn skills_config_from_state(state: &AppState) -> crate::skills::SkillsConfig {
    let config = state.config_manager.config();
    match &config.skills {
        Some(skills_toml) => crate::skills::SkillsConfig {
            project_dir: skills_toml.project_dir.clone(),
            personal_dir: skills_toml.personal_dir.clone(),
            enterprise_dir: None,
            plugin_dirs: skills_toml.plugin_dirs.clone().unwrap_or_default(),
        },
        None => crate::skills::SkillsConfig::default(),
    }
}
