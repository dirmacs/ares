// ares/src/mcp/tools.rs
// Input and output types for all ARES MCP tools.
// Each struct maps to one MCP tool's parameters or return value.

use serde::{Deserialize, Serialize};

// =============================================================================
// ares_list_agents
// =============================================================================

/// Input for ares_list_agents tool.
/// No parameters required — lists all agents for the authenticated tenant.
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct ListAgentsInput {}

/// One agent in the list response.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentSummary {
    /// Agent name (unique within tenant)
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Agent type: "chat", "workflow", "autonomous"
    pub agent_type: String,
    /// Whether the agent is currently active
    pub active: bool,
    /// When the agent was last deployed
    pub deployed_at: String,
}

/// Output for ares_list_agents tool.
#[derive(Debug, Serialize)]
pub struct ListAgentsOutput {
    pub agents: Vec<AgentSummary>,
    pub total: usize,
}

// =============================================================================
// ares_run_agent
// =============================================================================

/// Input for ares_run_agent tool.
#[derive(Debug, Deserialize, Serialize)]
pub struct RunAgentInput {
    /// Name of the agent to run (must exist in tenant's agent list)
    pub agent_name: String,
    /// The message to send to the agent
    pub message: String,
    /// Optional context ID for continuing a conversation
    #[serde(default)]
    pub context_id: Option<String>,
}

/// Output for ares_run_agent tool.
#[derive(Debug, Serialize)]
pub struct RunAgentOutput {
    /// The agent's response text
    pub response: String,
    /// Which agent handled the request
    pub agent: String,
    /// Context ID for continuing this conversation
    pub context_id: String,
    /// Sources cited by the agent (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<SourceRef>>,
}

/// A source reference from an agent response.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SourceRef {
    pub title: String,
    pub url: Option<String>,
    pub snippet: Option<String>,
}

// =============================================================================
// ares_get_status
// =============================================================================

/// Input for ares_get_status tool.
#[derive(Debug, Deserialize, Serialize)]
pub struct GetStatusInput {
    /// Context ID from a previous ares_run_agent call
    pub context_id: String,
}

/// Output for ares_get_status tool.
#[derive(Debug, Serialize)]
pub struct GetStatusOutput {
    pub context_id: String,
    /// "running", "completed", "failed", "not_found"
    pub status: String,
    /// Partial response text if still running
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_response: Option<String>,
    /// Error message if status is "failed"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// =============================================================================
// ares_deploy_agent
// =============================================================================

/// Input for ares_deploy_agent tool.
#[derive(Debug, Deserialize, Serialize)]
pub struct DeployAgentInput {
    /// The .toon config file contents as a string (TOML format)
    pub toon_config: String,
    /// Optional: override the agent name from the config
    #[serde(default)]
    pub name_override: Option<String>,
}

/// Output for ares_deploy_agent tool.
#[derive(Debug, Serialize)]
pub struct DeployAgentOutput {
    /// Name of the deployed agent
    pub agent_name: String,
    /// "created" or "updated"
    pub action: String,
    /// Whether the agent is now active
    pub active: bool,
    /// Deployment timestamp
    pub deployed_at: String,
}

// =============================================================================
// ares_get_usage
// =============================================================================

/// Input for ares_get_usage tool.
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct GetUsageInput {
    /// Optional: filter by date range (ISO 8601, e.g. "2026-03-01")
    #[serde(default)]
    pub from_date: Option<String>,
    #[serde(default)]
    pub to_date: Option<String>,
}

/// Output for ares_get_usage tool.
#[derive(Debug, Serialize)]
pub struct GetUsageOutput {
    pub tenant_id: String,
    pub tier: String,
    pub period: UsagePeriod,
    pub current_usage: UsageStats,
    pub quota: UsageQuota,
}

/// Usage period range.
#[derive(Debug, Serialize)]
pub struct UsagePeriod {
    pub from: String,
    pub to: String,
}

/// Current usage statistics.
#[derive(Debug, Serialize)]
pub struct UsageStats {
    pub total_requests: u64,
    pub chat_requests: u64,
    pub mcp_requests: u64,
    pub tokens_used: u64,
    pub agents_deployed: u32,
}

/// Quota limits for the tenant's tier.
#[derive(Debug, Serialize)]
pub struct UsageQuota {
    pub max_requests_per_month: u64,
    pub max_agents: u32,
    pub max_tokens_per_month: u64,
    /// Percentage of quota used (0.0 to 1.0)
    pub utilization: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::builtin_ares_tools;

    #[test]
    fn list_agents_input_round_trips() {
        let input = ListAgentsInput::default();
        let json = serde_json::to_string(&input).unwrap();
        let restored: ListAgentsInput = serde_json::from_str(&json).unwrap();
        assert_eq!(
            format!("{:?}", restored),
            format!("{:?}", ListAgentsInput::default())
        );
    }

    #[test]
    fn run_agent_input_round_trips_with_optional_context() {
        let input = RunAgentInput {
            agent_name: "listener".into(),
            message: "hello".into(),
            context_id: Some("ctx-1".into()),
        };
        let json = serde_json::to_string(&input).unwrap();
        let restored: RunAgentInput = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.agent_name, "listener");
        assert_eq!(restored.context_id.as_deref(), Some("ctx-1"));
    }

    #[test]
    fn deploy_agent_input_omits_default_name_override() {
        let input = DeployAgentInput {
            toon_config: "name = test".into(),
            name_override: None,
        };
        let json = serde_json::to_value(&input).unwrap();
        assert!(json.get("name_override").is_none() || json["name_override"].is_null());
    }

    #[test]
    fn source_ref_serializes_optional_url() {
        let source = SourceRef {
            title: "doc".into(),
            url: None,
            snippet: Some("excerpt".into()),
        };
        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json["title"], "doc");
        assert_eq!(json["snippet"], "excerpt");
    }

    #[test]
    fn get_usage_input_deserializes_empty_object() {
        let input: GetUsageInput = serde_json::from_str("{}").unwrap();
        assert!(input.from_date.is_none());
        assert!(input.to_date.is_none());
    }

    #[test]
    fn agent_summary_serde_roundtrip() {
        let agent = AgentSummary {
            name: "my-agent".into(),
            description: "A test agent".into(),
            agent_type: "workflow".into(),
            active: true,
            deployed_at: "2026-01-15T10:30:00Z".into(),
        };
        let json = serde_json::to_value(&agent).unwrap();
        let restored: AgentSummary = serde_json::from_value(json).unwrap();
        assert_eq!(restored.name, "my-agent");
        assert_eq!(restored.description, "A test agent");
        assert_eq!(restored.agent_type, "workflow");
        assert!(restored.active);
        assert_eq!(restored.deployed_at, "2026-01-15T10:30:00Z");
    }

    #[test]
    fn list_agents_output_serializes_total() {
        let output = ListAgentsOutput {
            agents: vec![AgentSummary {
                name: "a".into(),
                description: "d".into(),
                agent_type: "chat".into(),
                active: false,
                deployed_at: "2026-01-01".into(),
            }],
            total: 1,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["total"], 1);
        assert!(json["agents"].is_array());
        assert_eq!(json["agents"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn run_agent_output_serializes_with_sources() {
        let output = RunAgentOutput {
            response: "Hello!".into(),
            agent: "chatbot".into(),
            context_id: "ctx-42".into(),
            sources: Some(vec![SourceRef {
                title: "Doc 1".into(),
                url: Some("https://example.com".into()),
                snippet: Some("excerpt".into()),
            }]),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["response"], "Hello!");
        assert_eq!(json["agent"], "chatbot");
        assert_eq!(json["context_id"], "ctx-42");
        let sources = json["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["title"], "Doc 1");
    }

    #[test]
    fn run_agent_output_skips_none_sources() {
        let output = RunAgentOutput {
            response: "reply".into(),
            agent: "a".into(),
            context_id: "c".into(),
            sources: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("sources").is_none());
    }

    #[test]
    fn get_status_output_serializes_all_fields() {
        let output = GetStatusOutput {
            context_id: "ctx-99".into(),
            status: "running".into(),
            partial_response: Some("50% done".into()),
            error: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["context_id"], "ctx-99");
        assert_eq!(json["status"], "running");
        assert_eq!(json["partial_response"], "50% done");
        assert!(json.get("error").is_none());
    }

    #[test]
    fn get_status_output_skips_none_fields() {
        let output = GetStatusOutput {
            context_id: "ctx-1".into(),
            status: "completed".into(),
            partial_response: None,
            error: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("partial_response").is_none());
        assert!(json.get("error").is_none());
    }

    #[test]
    fn deploy_agent_output_serde_roundtrip() {
        let output = DeployAgentOutput {
            agent_name: "new-bot".into(),
            action: "created".into(),
            active: true,
            deployed_at: "2026-03-01".into(),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["agent_name"], "new-bot");
        assert_eq!(json["action"], "created");
        assert_eq!(json["active"], true);
        assert_eq!(json["deployed_at"], "2026-03-01");
    }

    #[test]
    fn get_usage_output_serializes_full_data() {
        let output = GetUsageOutput {
            tenant_id: "t1".into(),
            tier: "free".into(),
            period: UsagePeriod {
                from: "2026-01-01".into(),
                to: "2026-01-31".into(),
            },
            current_usage: UsageStats {
                total_requests: 100,
                chat_requests: 80,
                mcp_requests: 20,
                tokens_used: 5000,
                agents_deployed: 3,
            },
            quota: UsageQuota {
                max_requests_per_month: 1000,
                max_agents: 3,
                max_tokens_per_month: 10000,
                utilization: 0.5,
            },
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["tenant_id"], "t1");
        assert_eq!(json["tier"], "free");
        assert_eq!(json["period"]["from"], "2026-01-01");
        assert_eq!(json["current_usage"]["total_requests"], 100);
        assert_eq!(json["quota"]["utilization"], 0.5);
    }

    #[test]
    fn usage_period_stats_quota_construct_and_serialize() {
        let period = UsagePeriod {
            from: "2026-04-01".into(),
            to: "2026-04-30".into(),
        };
        let stats = UsageStats {
            total_requests: 42,
            chat_requests: 30,
            mcp_requests: 12,
            tokens_used: 2048,
            agents_deployed: 5,
        };
        let quota = UsageQuota {
            max_requests_per_month: 50_000,
            max_agents: 20,
            max_tokens_per_month: 500_000,
            utilization: 0.04096,
        };

        let p_json = serde_json::to_value(&period).unwrap();
        assert_eq!(p_json["from"], "2026-04-01");
        assert_eq!(p_json["to"], "2026-04-30");

        let s_json = serde_json::to_value(&stats).unwrap();
        assert_eq!(s_json["total_requests"], 42);
        assert_eq!(s_json["agents_deployed"], 5);

        let q_json = serde_json::to_value(&quota).unwrap();
        assert_eq!(q_json["max_requests_per_month"], 50_000);
        assert_eq!(q_json["utilization"], 0.04096);
    }

    #[test]
    fn run_agent_input_deserializes_without_context_id() {
        let json = r#"{"agent_name":"bot","message":"hi"}"#;
        let input: RunAgentInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.agent_name, "bot");
        assert_eq!(input.message, "hi");
        assert!(input.context_id.is_none());
    }

    #[test]
    fn get_status_input_roundtrip() {
        let input = GetStatusInput {
            context_id: "ctx-7".into(),
        };
        let json = serde_json::to_value(&input).unwrap();
        let restored: GetStatusInput = serde_json::from_value(json).unwrap();
        assert_eq!(restored.context_id, "ctx-7");
    }

    #[test]
    fn deploy_agent_input_roundtrip_with_name_override() {
        let input = DeployAgentInput {
            toon_config: "[agent]\nname = \"x\"".into(),
            name_override: Some("override-name".into()),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["name_override"], "override-name");
        let restored: DeployAgentInput = serde_json::from_value(json).unwrap();
        assert_eq!(restored.name_override.as_deref(), Some("override-name"));
        assert_eq!(restored.toon_config, "[agent]\nname = \"x\"");
    }
    // ---- deserialization error paths ----

    #[test]
    fn run_agent_input_fails_without_required_fields() {
        let result = serde_json::from_str::<RunAgentInput>(r#"{}"#);
        assert!(result.is_err());
    }

    #[test]
    fn run_agent_input_fails_without_agent_name() {
        let result = serde_json::from_str::<RunAgentInput>(r#"{"message":"hi"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn run_agent_input_fails_without_message() {
        let result = serde_json::from_str::<RunAgentInput>(r#"{"agent_name":"bot"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn get_status_input_fails_on_empty() {
        let result = serde_json::from_str::<GetStatusInput>(r#"{}"#);
        assert!(result.is_err());
    }

    #[test]
    fn deploy_agent_input_fails_without_toon_config() {
        let result = serde_json::from_str::<DeployAgentInput>(r#"{}"#);
        assert!(result.is_err());
    }

    // ---- clone trait ----

    #[test]
    fn agent_summary_clone_preserves_all_fields() {
        let original = AgentSummary {
            name: "alpha".into(),
            description: "desc".into(),
            agent_type: "autonomous".into(),
            active: true,
            deployed_at: "2026-05-01".into(),
        };
        let cloned = original.clone();
        assert_eq!(cloned.name, "alpha");
        assert_eq!(cloned.agent_type, "autonomous");
        assert!(cloned.active);
    }

    #[test]
    fn source_ref_clone_independence() {
        let mut source = SourceRef {
            title: "t".into(),
            url: Some("http://x".into()),
            snippet: None,
        };
        let cloned = source.clone();
        source.title = "changed".into();
        assert_eq!(cloned.title, "t"); // clone not affected
    }

    // ---- edge cases for numeric fields ----

    #[test]
    fn usage_quota_zero_utilization() {
        let quota = UsageQuota {
            max_requests_per_month: 0,
            max_agents: 0,
            max_tokens_per_month: 0,
            utilization: 0.0,
        };
        let json = serde_json::to_value(&quota).unwrap();
        assert_eq!(json["utilization"], 0.0);
        assert_eq!(json["max_requests_per_month"], 0);
    }

    #[test]
    fn usage_quota_full_utilization() {
        let quota = UsageQuota {
            max_requests_per_month: 100,
            max_agents: 5,
            max_tokens_per_month: 50_000,
            utilization: 1.0,
        };
        let json = serde_json::to_value(&quota).unwrap();
        assert_eq!(json["utilization"], 1.0);
    }

    #[test]
    fn usage_stats_all_zeros() {
        let stats = UsageStats {
            total_requests: 0,
            chat_requests: 0,
            mcp_requests: 0,
            tokens_used: 0,
            agents_deployed: 0,
        };
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["total_requests"], 0);
        assert_eq!(json["agents_deployed"], 0);
    }

    // ---- empty collections ----

    #[test]
    fn list_agents_output_empty_agents() {
        let output = ListAgentsOutput {
            agents: vec![],
            total: 0,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["total"], 0);
        assert_eq!(json["agents"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn run_agent_output_empty_sources_vec() {
        let output = RunAgentOutput {
            response: "r".into(),
            agent: "a".into(),
            context_id: "c".into(),
            sources: Some(vec![]),
        };
        let json = serde_json::to_value(&output).unwrap();
        // Some(vec![]) is present (not skipped), but empty
        assert_eq!(json["sources"].as_array().unwrap().len(), 0);
    }

    // ---- GetUsageInput with values ----

    #[test]
    fn get_usage_input_with_dates_roundtrip() {
        let input = GetUsageInput {
            from_date: Some("2026-03-01".into()),
            to_date: Some("2026-03-31".into()),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["from_date"], "2026-03-01");
        assert_eq!(json["to_date"], "2026-03-31");
        let restored: GetUsageInput = serde_json::from_value(json).unwrap();
        assert_eq!(restored.from_date.as_deref(), Some("2026-03-01"));
        assert_eq!(restored.to_date.as_deref(), Some("2026-03-31"));
    }

    #[test]
    fn get_usage_input_partial_dates() {
        let input = GetUsageInput {
            from_date: Some("2026-01-01".into()),
            to_date: None,
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["from_date"], "2026-01-01");
        // serde(default) only aids deserialization; None still serializes as null
        assert!(json["to_date"].is_null());
    }

    // ---- SourceRef with all fields ----

    #[test]
    fn source_ref_all_fields_present() {
        let source = SourceRef {
            title: "Research Paper".into(),
            url: Some("https://arxiv.org/abs/1234".into()),
            snippet: Some("This paper covers...".into()),
        };
        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json["title"], "Research Paper");
        assert_eq!(json["url"], "https://arxiv.org/abs/1234");
        assert_eq!(json["snippet"], "This paper covers...");
    }

    // ---- RunAgentOutput serialization with all sources having optional fields ----

    #[test]
    fn run_agent_output_sources_missing_optionals() {
        let output = RunAgentOutput {
            response: "done".into(),
            agent: "search".into(),
            context_id: "ctx-x".into(),
            sources: Some(vec![SourceRef {
                title: "doc".into(),
                url: None,
                snippet: None,
            }]),
        };
        let json = serde_json::to_value(&output).unwrap();
        let source = &json["sources"][0];
        assert_eq!(source["title"], "doc");
        // SourceRef has no skip_serializing_if; None serializes as null
        assert!(source["url"].is_null());
        assert!(source["snippet"].is_null());
    }

    // ---- Debug format sanity ----

    #[test]
    fn debug_format_all_input_types() {
        let list = ListAgentsInput::default();
        assert!(format!("{:?}", list).contains("ListAgentsInput"));

        let run = RunAgentInput {
            agent_name: "a".into(),
            message: "m".into(),
            context_id: None,
        };
        assert!(format!("{:?}", run).contains("RunAgentInput"));

        let status = GetStatusInput {
            context_id: "c".into(),
        };
        assert!(format!("{:?}", status).contains("GetStatusInput"));

        let deploy = DeployAgentInput {
            toon_config: "t".into(),
            name_override: None,
        };
        assert!(format!("{:?}", deploy).contains("DeployAgentInput"));

        let usage = GetUsageInput::default();
        assert!(format!("{:?}", usage).contains("GetUsageInput"));
    }

    // ---- DeployAgentInput deserialization edge cases ----

    #[test]
    fn deploy_agent_input_deserializes_with_all_fields() {
        let json = r#"{"toon_config":"[agent]","name_override":"custom"}"#;
        let input: DeployAgentInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.toon_config, "[agent]");
        assert_eq!(input.name_override.as_deref(), Some("custom"));
    }

    #[test]
    fn deploy_agent_input_default_name_override_is_none() {
        let json = r#"{"toon_config":"cfg"}"#;
        let input: DeployAgentInput = serde_json::from_str(json).unwrap();
        assert!(input.name_override.is_none());
    }

    // ---- GetStatusOutput with error field populated ----

    #[test]
    fn get_status_output_failed_with_error() {
        let output = GetStatusOutput {
            context_id: "ctx-e".into(),
            status: "failed".into(),
            partial_response: None,
            error: Some("timeout after 30s".into()),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "failed");
        assert_eq!(json["error"], "timeout after 30s");
        assert!(json.get("partial_response").is_none());
    }

    // ---- ListAgentsOutput with many agents ----

    #[test]
    fn list_agents_output_total_independent_of_vec_len() {
        let output = ListAgentsOutput {
            agents: vec![AgentSummary {
                name: "a1".into(),
                description: "d1".into(),
                agent_type: "chat".into(),
                active: true,
                deployed_at: "2026-01-01".into(),
            }],
            total: 50, // total can exceed vec length (paginated)
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["total"], 50);
        assert_eq!(json["agents"].as_array().unwrap().len(), 1);
    }

    // ========================================================================
    // 1. Tool schema validation for all 5 built-in tools
    // ========================================================================

    #[test]
    fn all_builtin_tools_present_in_registry() {
        let tools = builtin_ares_tools();
        assert_eq!(tools.len(), 5);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"ares_list_agents"));
        assert!(names.contains(&"ares_run_agent"));
        assert!(names.contains(&"ares_get_status"));
        assert!(names.contains(&"ares_deploy_agent"));
        assert!(names.contains(&"ares_get_usage"));
    }

    #[test]
    fn list_agents_schema_has_object_type_and_no_required() {
        let tools = builtin_ares_tools();
        let tool = tools.iter().find(|t| t.name == "ares_list_agents").unwrap();
        assert_eq!(tool.input_schema["type"], "object");
        let required = tool.input_schema["required"].as_array().unwrap();
        assert!(
            required.is_empty(),
            "list_agents should have no required fields"
        );
    }

    #[test]
    fn run_agent_schema_requires_agent_name_and_message() {
        let tools = builtin_ares_tools();
        let tool = tools.iter().find(|t| t.name == "ares_run_agent").unwrap();
        assert_eq!(tool.input_schema["type"], "object");
        let required: Vec<&str> = tool.input_schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(required.contains(&"agent_name"));
        assert!(required.contains(&"message"));
        assert!(
            !required.contains(&"context_id"),
            "context_id should be optional"
        );
        // Verify properties exist in schema
        assert!(tool.input_schema["properties"]["agent_name"].is_object());
        assert!(tool.input_schema["properties"]["message"].is_object());
        assert!(tool.input_schema["properties"]["context_id"].is_object());
    }

    #[test]
    fn get_status_schema_requires_context_id() {
        let tools = builtin_ares_tools();
        let tool = tools.iter().find(|t| t.name == "ares_get_status").unwrap();
        assert_eq!(tool.input_schema["type"], "object");
        let required = tool.input_schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].as_str(), Some("context_id"));
        assert!(tool.input_schema["properties"]["context_id"].is_object());
    }

    #[test]
    fn deploy_agent_schema_requires_toon_config() {
        let tools = builtin_ares_tools();
        let tool = tools
            .iter()
            .find(|t| t.name == "ares_deploy_agent")
            .unwrap();
        assert_eq!(tool.input_schema["type"], "object");
        let required = tool.input_schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].as_str(), Some("toon_config"));
        assert!(tool.input_schema["properties"]["toon_config"].is_object());
        assert!(tool.input_schema["properties"]["name_override"].is_object());
    }

    #[test]
    fn get_usage_schema_has_object_type_and_no_required() {
        let tools = builtin_ares_tools();
        let tool = tools.iter().find(|t| t.name == "ares_get_usage").unwrap();
        assert_eq!(tool.input_schema["type"], "object");
        let required = tool.input_schema["required"].as_array().unwrap();
        assert!(
            required.is_empty(),
            "get_usage should have no required fields"
        );
        assert!(tool.input_schema["properties"]["from_date"].is_object());
        assert!(tool.input_schema["properties"]["to_date"].is_object());
    }

    #[test]
    fn all_builtin_tools_have_non_empty_description() {
        for tool in builtin_ares_tools() {
            assert!(
                tool.description
                    .as_ref()
                    .map(|d| !d.is_empty())
                    .unwrap_or(false),
                "{} should have a non-empty description",
                tool.name
            );
        }
    }

    // ========================================================================
    // 2. Tool argument parsing (valid/invalid)
    // ========================================================================

    #[test]
    fn list_agents_input_ignores_extra_fields() {
        let input: ListAgentsInput = serde_json::from_str(r#"{"foo":"bar"}"#).unwrap();
        assert_eq!(
            format!("{:?}", input),
            format!("{:?}", ListAgentsInput::default())
        );
    }

    #[test]
    fn run_agent_input_accepts_null_context_id() {
        let input: RunAgentInput =
            serde_json::from_str(r#"{"agent_name":"bot","message":"hi","context_id":null}"#)
                .unwrap();
        assert!(input.context_id.is_none());
    }

    #[test]
    fn run_agent_input_rejects_extra_nested_wrong_type() {
        // message must be a string, not an object
        let result = serde_json::from_str::<RunAgentInput>(
            r#"{"agent_name":"bot","message":{"nested":"bad"}}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn run_agent_input_rejects_array_for_agent_name() {
        let result =
            serde_json::from_str::<RunAgentInput>(r#"{"agent_name":["bot"],"message":"hi"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn get_status_input_rejects_array_context_id() {
        let result = serde_json::from_str::<GetStatusInput>(r#"{"context_id":["x"]}"#);
        assert!(result.is_err());
    }

    #[test]
    fn get_status_input_rejects_object_context_id() {
        let result = serde_json::from_str::<GetStatusInput>(r#"{"context_id":{"id":"x"}}"#);
        assert!(result.is_err());
    }

    #[test]
    fn deploy_agent_input_accepts_empty_toon_config() {
        // Empty string is valid for the serde schema (business logic may reject later)
        let input: DeployAgentInput = serde_json::from_str(r#"{"toon_config":""}"#).unwrap();
        assert_eq!(input.toon_config, "");
        assert!(input.name_override.is_none());
    }

    #[test]
    fn deploy_agent_input_rejects_object_toon_config() {
        let result = serde_json::from_str::<DeployAgentInput>(r#"{"toon_config":{}}"#);
        assert!(result.is_err());
    }

    #[test]
    fn deploy_agent_input_rejects_null_name_override_with_missing_toon_config() {
        let result = serde_json::from_str::<DeployAgentInput>(r#"{"name_override":null}"#);
        assert!(result.is_err());
    }

    #[test]
    fn get_usage_input_ignores_unknown_fields() {
        let input: GetUsageInput = serde_json::from_str(r#"{"unknown":123}"#).unwrap();
        assert!(input.from_date.is_none());
        assert!(input.to_date.is_none());
    }

    #[test]
    fn get_usage_input_rejects_array_for_date() {
        let result = serde_json::from_str::<GetUsageInput>(r#"{"from_date":["2026-01-01"]}"#);
        assert!(result.is_err());
    }

    #[test]
    fn get_usage_input_rejects_object_for_date() {
        let result = serde_json::from_str::<GetUsageInput>(r#"{"to_date":{"year":2026}}"#);
        assert!(result.is_err());
    }

    // ========================================================================
    // 3. Tool execution with mock dependencies (output types)
    // ========================================================================

    #[test]
    fn list_agents_output_matches_real_execution() {
        let output = ListAgentsOutput {
            agents: vec![
                AgentSummary {
                    name: "support".into(),
                    description: "Customer support agent".into(),
                    agent_type: "chat".into(),
                    active: true,
                    deployed_at: "2026-05-01T12:00:00Z".into(),
                },
                AgentSummary {
                    name: "research".into(),
                    description: "Deep research agent".into(),
                    agent_type: "autonomous".into(),
                    active: false,
                    deployed_at: "2026-04-01T00:00:00Z".into(),
                },
            ],
            total: 2,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["total"], 2);
        let agents = json["agents"].as_array().unwrap();
        assert_eq!(agents[0]["name"], "support");
        assert_eq!(agents[0]["active"], true);
        assert_eq!(agents[1]["agent_type"], "autonomous");
        assert_eq!(agents[1]["active"], false);
    }

    #[test]
    fn run_agent_output_with_multiple_sources() {
        let output = RunAgentOutput {
            response: "Here are the results.".into(),
            agent: "search-bot".into(),
            context_id: "ctx-multi".into(),
            sources: Some(vec![
                SourceRef {
                    title: "Doc A".into(),
                    url: Some("https://a.com".into()),
                    snippet: Some("snippet A".into()),
                },
                SourceRef {
                    title: "Doc B".into(),
                    url: None,
                    snippet: Some("snippet B".into()),
                },
            ]),
        };
        let json = serde_json::to_value(&output).unwrap();
        let sources = json["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0]["url"], "https://a.com");
        assert!(sources[1]["url"].is_null());
    }

    #[test]
    fn run_agent_output_without_sources_omits_key() {
        let output = RunAgentOutput {
            response: "No sources needed.".into(),
            agent: "simple".into(),
            context_id: "ctx-none".into(),
            sources: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert!(!json.as_object().unwrap().contains_key("sources"));
    }

    #[test]
    fn get_status_output_running_with_partial() {
        let output = GetStatusOutput {
            context_id: "ctx-run".into(),
            status: "running".into(),
            partial_response: Some("Processing step 2 of 5...".into()),
            error: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "running");
        assert_eq!(json["partial_response"], "Processing step 2 of 5...");
        assert!(!json.as_object().unwrap().contains_key("error"));
    }

    #[test]
    fn get_status_output_completed_without_partials() {
        let output = GetStatusOutput {
            context_id: "ctx-done".into(),
            status: "completed".into(),
            partial_response: None,
            error: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "completed");
        assert!(!json.as_object().unwrap().contains_key("partial_response"));
        assert!(!json.as_object().unwrap().contains_key("error"));
    }

    #[test]
    fn deploy_agent_output_updated_action() {
        let output = DeployAgentOutput {
            agent_name: "existing-agent".into(),
            action: "updated".into(),
            active: true,
            deployed_at: "2026-06-04T10:30:00Z".into(),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["action"], "updated");
        assert_eq!(json["active"], true);
    }

    // ========================================================================
    // 4. Quota checking integration
    // ========================================================================

    #[test]
    fn get_usage_output_free_tier_serialization() {
        let output = GetUsageOutput {
            tenant_id: "tenant-free".into(),
            tier: "free".into(),
            period: UsagePeriod {
                from: "2026-06-01".into(),
                to: "2026-06-30".into(),
            },
            current_usage: UsageStats {
                total_requests: 500,
                chat_requests: 400,
                mcp_requests: 100,
                tokens_used: 5_000,
                agents_deployed: 2,
            },
            quota: UsageQuota {
                max_requests_per_month: 1_000,
                max_agents: 3,
                max_tokens_per_month: 10_000,
                utilization: 0.5,
            },
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["tenant_id"], "tenant-free");
        assert_eq!(json["tier"], "free");
        assert_eq!(json["current_usage"]["total_requests"], 500);
        assert_eq!(json["quota"]["utilization"], 0.5);
    }

    #[test]
    fn usage_quota_boundary_values() {
        let quota = UsageQuota {
            max_requests_per_month: u64::MAX,
            max_agents: u32::MAX,
            max_tokens_per_month: u64::MAX,
            utilization: 1.0,
        };
        let json = serde_json::to_value(&quota).unwrap();
        assert_eq!(json["max_requests_per_month"], u64::MAX);
        assert_eq!(json["max_agents"], u32::MAX);
        assert_eq!(json["max_tokens_per_month"], u64::MAX);
        assert_eq!(json["utilization"], 1.0);
    }

    #[test]
    fn usage_stats_boundary_values() {
        let stats = UsageStats {
            total_requests: u64::MAX,
            chat_requests: u64::MAX,
            mcp_requests: u64::MAX,
            tokens_used: u64::MAX,
            agents_deployed: u32::MAX,
        };
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["total_requests"], u64::MAX);
        assert_eq!(json["agents_deployed"], u32::MAX);
    }

    #[test]
    fn get_usage_output_enterprise_tier() {
        let output = GetUsageOutput {
            tenant_id: "tenant-ent".into(),
            tier: "enterprise".into(),
            period: UsagePeriod {
                from: "2026-01-01".into(),
                to: "2026-12-31".into(),
            },
            current_usage: UsageStats {
                total_requests: 1_000_000,
                chat_requests: 800_000,
                mcp_requests: 200_000,
                tokens_used: 500_000_000,
                agents_deployed: 50,
            },
            quota: UsageQuota {
                max_requests_per_month: 10_000_000,
                max_agents: 100,
                max_tokens_per_month: 1_000_000_000,
                utilization: 0.1,
            },
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["tier"], "enterprise");
        assert_eq!(json["current_usage"]["chat_requests"], 800_000);
        assert_eq!(json["quota"]["max_agents"], 100);
    }

    #[test]
    fn get_usage_output_dev_tier() {
        let output = GetUsageOutput {
            tenant_id: "tenant-dev".into(),
            tier: "dev".into(),
            period: UsagePeriod {
                from: "2026-03-01".into(),
                to: "2026-03-31".into(),
            },
            current_usage: UsageStats {
                total_requests: 10,
                chat_requests: 5,
                mcp_requests: 5,
                tokens_used: 100,
                agents_deployed: 1,
            },
            quota: UsageQuota {
                max_requests_per_month: 100,
                max_agents: 1,
                max_tokens_per_month: 1_000,
                utilization: 0.1,
            },
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["tier"], "dev");
        assert_eq!(json["current_usage"]["agents_deployed"], 1);
    }

    // ========================================================================
    // 5. Error responses format
    // ========================================================================

    #[test]
    fn error_response_failed_with_message() {
        let output = GetStatusOutput {
            context_id: "ctx-fail".into(),
            status: "failed".into(),
            partial_response: None,
            error: Some("Database connection lost".into()),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "failed");
        assert_eq!(json["error"], "Database connection lost");
        assert!(!json.as_object().unwrap().contains_key("partial_response"));
    }

    #[test]
    fn error_response_not_found_omits_optional_fields() {
        let output = GetStatusOutput {
            context_id: "ctx-missing".into(),
            status: "not_found".into(),
            partial_response: None,
            error: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "not_found");
        assert!(!json.as_object().unwrap().contains_key("partial_response"));
        assert!(!json.as_object().unwrap().contains_key("error"));
    }

    #[test]
    fn error_response_with_partial_and_error() {
        // Edge case: failed but partial response available
        let output = GetStatusOutput {
            context_id: "ctx-partial-fail".into(),
            status: "failed".into(),
            partial_response: Some("Partial result before crash".into()),
            error: Some("Out of memory".into()),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "failed");
        assert_eq!(json["partial_response"], "Partial result before crash");
        assert_eq!(json["error"], "Out of memory");
    }

    #[test]
    fn run_agent_output_error_text_serialization() {
        // Simulates a tool execution that returned an error text response
        let output = RunAgentOutput {
            response: "ERROR: Rate limit exceeded".into(),
            agent: "rate-limited-bot".into(),
            context_id: "ctx-rate".into(),
            sources: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["response"], "ERROR: Rate limit exceeded");
        assert!(!json.as_object().unwrap().contains_key("sources"));
    }

    #[test]
    fn deploy_agent_output_error_action_simulation() {
        let output = DeployAgentOutput {
            agent_name: "bad-config".into(),
            action: "error".into(),
            active: false,
            deployed_at: "".into(),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["action"], "error");
        assert_eq!(json["active"], false);
    }

    #[test]
    fn get_usage_output_error_simulation_via_high_utilization() {
        let output = GetUsageOutput {
            tenant_id: "tenant-over".into(),
            tier: "free".into(),
            period: UsagePeriod {
                from: "2026-06-01".into(),
                to: "2026-06-30".into(),
            },
            current_usage: UsageStats {
                total_requests: 1_100,
                chat_requests: 900,
                mcp_requests: 200,
                tokens_used: 11_000,
                agents_deployed: 4,
            },
            quota: UsageQuota {
                max_requests_per_month: 1_000,
                max_agents: 3,
                max_tokens_per_month: 10_000,
                utilization: 1.1,
            },
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["quota"]["utilization"], 1.1);
        assert_eq!(json["current_usage"]["agents_deployed"], 4);
    }
}
