//! Backend-neutral domain types for the store.
//!
//! These types are the vocabulary of [`crate::traits::DatabaseClient`]. They
//! describe the shape of a user, a conversation, and an agent regardless of
//! which backend persists them, so they must not name or depend on a specific
//! backend module — otherwise the interface drags that backend into every build.
//!
//! Row mapping lives with each backend. The Postgres client derives
//! `sqlx::FromRow` on mirror rows inside `postgres.rs`; the embedded client reads
//! columns positionally with `row.get(n)`. Neither concern belongs here.

use serde::{Deserialize, Serialize};

/// JSON configuration stored in `UserAgent::extra`.
///
/// `Default` is implemented by hand rather than derived: the serde attribute on
/// `max_tool_iterations` only applies when deserializing, so a derived `Default`
/// would silently disagree with what `serde_json::from_str("{}")` produces
/// (0 versus 10). This bit a test, so it is fixed at source rather than pinned.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserAgentExtraConfig {
    #[serde(default)]
    pub parallel_tools: bool,
    #[serde(default = "default_max_tool_iterations")]
    pub max_tool_iterations: i32,
    #[serde(default)]
    pub custom: serde_json::Map<String, serde_json::Value>,
}

impl Default for UserAgentExtraConfig {
    fn default() -> Self {
        Self {
            parallel_tools: false,
            max_tool_iterations: default_max_tool_iterations(),
            custom: serde_json::Map::new(),
        }
    }
}

fn default_max_tool_iterations() -> i32 {
    10
}

impl UserAgentExtraConfig {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone)]
pub struct Conversation {
    pub id: String,
    pub user_id: String,
    pub title: Option<String>,
    /// Populated by backend queries that join message counts; defaults to 0.
    pub message_count: i32,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    pub email: String,
    pub password_hash: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct UserAgent {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub model: String,
    pub system_prompt: Option<String>,
    pub tools: String,
    pub max_tool_iterations: i32,
    pub parallel_tools: bool,
    pub extra: String,
    pub is_public: bool,
    pub usage_count: i32,
    pub rating_sum: i32,
    pub rating_count: i32,
    pub created_at: i64,
    pub updated_at: i64,
}

impl UserAgent {
    pub fn new(id: String, user_id: String, name: String, model: String) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            id,
            user_id,
            name,
            display_name: None,
            description: None,
            model,
            system_prompt: None,
            tools: "[]".to_string(),
            max_tool_iterations: 10,
            parallel_tools: false,
            extra: "{}".to_string(),
            is_public: false,
            usage_count: 0,
            rating_sum: 0,
            rating_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn tools_vec(&self) -> Vec<String> {
        serde_json::from_str(&self.tools).unwrap_or_default()
    }

    pub fn set_tools(&mut self, tools: Vec<String>) {
        self.tools = serde_json::to_string(&tools).unwrap_or_else(|_| "[]".to_string());
    }

    pub fn average_rating(&self) -> Option<f32> {
        if self.rating_count > 0 {
            Some(self.rating_sum as f32 / self.rating_count as f32)
        } else {
            None
        }
    }

    pub fn extra_config(&self) -> UserAgentExtraConfig {
        serde_json::from_str(&self.extra).unwrap_or_default()
    }

    pub fn set_extra_config(&mut self, config: &UserAgentExtraConfig) {
        self.extra = serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string());
        self.parallel_tools = config.parallel_tools;
        self.max_tool_iterations = config.max_tool_iterations;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_new_defaults_are_consistent() {
        let a = UserAgent::new("id1".into(), "u1".into(), "n".into(), "m".into());
        assert_eq!(a.tools_vec(), Vec::<String>::new());
        assert_eq!(a.average_rating(), None);
        assert_eq!(a.extra_config(), UserAgentExtraConfig::default());
        assert!(!a.is_public);
    }

    #[test]
    fn user_agent_tools_round_trip() {
        let mut a = UserAgent::new("id1".into(), "u1".into(), "n".into(), "m".into());
        a.set_tools(vec!["shell".into(), "fetch".into()]);
        assert_eq!(
            a.tools_vec(),
            vec!["shell".to_string(), "fetch".to_string()]
        );
    }

    #[test]
    fn user_agent_extra_config_round_trip_sets_mirrored_fields() {
        let mut a = UserAgent::new("id1".into(), "u1".into(), "n".into(), "m".into());
        let cfg = UserAgentExtraConfig {
            parallel_tools: true,
            max_tool_iterations: 42,
            custom: serde_json::Map::new(),
        };
        a.set_extra_config(&cfg);
        assert!(a.parallel_tools);
        assert_eq!(a.max_tool_iterations, 42);
        assert_eq!(a.extra_config(), cfg);
    }

    #[test]
    fn user_agent_average_rating_only_when_rated() {
        let mut a = UserAgent::new("id1".into(), "u1".into(), "n".into(), "m".into());
        a.rating_sum = 9;
        a.rating_count = 3;
        assert_eq!(a.average_rating(), Some(3.0));
    }
}
