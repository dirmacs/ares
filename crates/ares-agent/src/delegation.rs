//! The tenant agent `agents` config key: one agent calling other agents.
//!
//! A *manager* names the *specialists* it may call in its tenant agent JSON
//! config; [`delegation_entries_from_json`] parses and refuses bad values so
//! a save cannot store them, and [`delegation_tool_definitions`] turns the
//! entries into the tool definitions the manager's model sees.
//!
//! Running a delegation, its guardrails and its run rows are separate work
//! (plan §5.3, §5.4). Nothing here looks agents up: a manager may be saved
//! before its specialists, and missing or disabled ones are skipped when the
//! definitions are built.

use std::collections::HashMap;

use ares_types::types::{AppError, Result, ToolDefinition};
use serde::{Deserialize, Serialize};

/// Specialists one manager may be given.
pub const MAX_AGENTS_PER_MANAGER: usize = 8;

/// Longest `when_to_use` the model is shown, in characters.
pub const MAX_WHEN_TO_USE_CHARS: usize = 300;

/// Every delegation tool name starts here, so no other tool name can collide.
pub const AGENT_TOOL_PREFIX: &str = "agent__";

/// Provider tool names are limited to 64 characters; the prefix takes 7.
pub const MAX_AGENT_TOOL_NAME_CHARS: usize = 64;

/// One specialist a manager may call, and when to call it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationEntry {
    /// `agent_name` of another tenant agent of the same tenant.
    pub name: String,
    /// When the manager should call this specialist; becomes the tool description.
    pub when_to_use: String,
}

/// Parses the optional `agents` key of a tenant agent config.
///
/// `agents` must be an array of at most [`MAX_AGENTS_PER_MANAGER`] objects,
/// each holding exactly `name` and `when_to_use`; any other key is refused, so
/// a typo cannot be silently ignored. Values are trimmed and stored trimmed.
/// The agent's own name, duplicate names and tool names that collide after
/// sanitising are refused too. Every refusal is [`AppError::InvalidInput`] and
/// names the offending entry, so an operator can find it.
///
/// The named agents are not looked up: a manager may be saved before its
/// specialists, and ones that are missing or disabled are skipped at run time.
pub fn delegation_entries_from_json(
    config: &serde_json::Value,
    own_agent_name: &str,
) -> Result<Vec<DelegationEntry>> {
    let raw = match config.get("agents") {
        None | Some(serde_json::Value::Null) => return Ok(Vec::new()),
        Some(raw) => raw,
    };

    let items = raw.as_array().ok_or_else(|| {
        invalid(format!(
            "'agents' must be an array of objects, was {}",
            json_kind(raw)
        ))
    })?;

    if items.len() > MAX_AGENTS_PER_MANAGER {
        let overflow = entry_label(
            MAX_AGENTS_PER_MANAGER,
            entry_name(&items[MAX_AGENTS_PER_MANAGER]),
        );
        return Err(invalid(format!(
            "{overflow}: 'agents' holds {} entries, the limit is {MAX_AGENTS_PER_MANAGER}",
            items.len()
        )));
    }

    let own_name = own_agent_name.trim();
    let mut entries: Vec<DelegationEntry> = Vec::with_capacity(items.len());
    let mut names: Vec<String> = Vec::with_capacity(items.len());
    let mut tool_names: Vec<(String, String)> = Vec::with_capacity(items.len());

    for (index, item) in items.iter().enumerate() {
        let obj = item.as_object().ok_or_else(|| {
            invalid(format!(
                "{} must be an object, was {}",
                entry_label(index, ""),
                json_kind(item)
            ))
        })?;

        let label = entry_label(index, entry_name(item));

        for key in obj.keys() {
            if key != "name" && key != "when_to_use" {
                return Err(invalid(format!(
                    "{label}: unknown key '{key}'; an entry holds exactly 'name' and 'when_to_use'"
                )));
            }
        }

        let name = obj
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| invalid(format!("{label}: 'name' must be a non-empty string")))?
            .to_string();

        if name.eq_ignore_ascii_case(own_name) {
            return Err(invalid(format!(
                "{label}: an agent cannot be given itself as a specialist"
            )));
        }

        let when_to_use = obj
            .get("when_to_use")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|when| !when.is_empty())
            .ok_or_else(|| invalid(format!("{label}: 'when_to_use' must be a non-empty string")))
            .and_then(|when| {
                let chars = when.chars().count();
                if chars > MAX_WHEN_TO_USE_CHARS {
                    Err(invalid(format!(
                        "{label}: 'when_to_use' is {chars} characters, the limit is {MAX_WHEN_TO_USE_CHARS}"
                    )))
                } else {
                    Ok(when.to_string())
                }
            })?;

        if names.iter().any(|seen| seen == &name) {
            return Err(invalid(format!(
                "{label}: duplicate specialist; each one may be named once"
            )));
        }

        let tool_name = agent_tool_name(&name);
        if let Some((_, first)) = tool_names.iter().find(|(tool, _)| tool == &tool_name) {
            return Err(invalid(format!(
                "{label}: tool name '{tool_name}' collides with specialist '{first}'; rename it"
            )));
        }
        if tool_name.chars().count() > MAX_AGENT_TOOL_NAME_CHARS {
            return Err(invalid(format!(
                "{label}: tool name '{tool_name}' is {} characters, the limit is {MAX_AGENT_TOOL_NAME_CHARS}",
                tool_name.chars().count()
            )));
        }

        names.push(name.clone());
        tool_names.push((tool_name, name.clone()));
        entries.push(DelegationEntry { name, when_to_use });
    }

    Ok(entries)
}

/// The tool name the model calls for one specialist.
///
/// The prefix plus the agent name lower-cased, with every character outside
/// `[a-z0-9_]` replaced by `_`, so `Price-Analyst` becomes `agent__price_analyst`.
/// Names are sanitised, never rejected, so any `agent_name` stays reachable.
pub fn agent_tool_name(agent_name: &str) -> String {
    let mut tool_name = String::with_capacity(AGENT_TOOL_PREFIX.len() + agent_name.len());
    tool_name.push_str(AGENT_TOOL_PREFIX);
    for ch in agent_name.trim().to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' {
            tool_name.push(ch);
        } else {
            tool_name.push('_');
        }
    }
    tool_name
}

/// Whether a tool name is one a manager calls a specialist through.
pub fn is_agent_tool_name(tool_name: &str) -> bool {
    tool_name.starts_with(AGENT_TOOL_PREFIX)
}

/// One tool definition per entry, in entry order.
///
/// `display_names` maps an available specialist's `agent_name` to its display
/// name; the caller fills it from the agents that exist and are enabled. An
/// entry missing from the map is skipped without an error, so a manager keeps
/// working while one of its specialists is off.
pub fn delegation_tool_definitions(
    entries: &[DelegationEntry],
    display_names: &HashMap<String, String>,
) -> Vec<ToolDefinition> {
    entries
        .iter()
        .filter_map(|entry| {
            let display_name = display_names.get(entry.name.as_str())?;
            Some(ToolDefinition {
                name: agent_tool_name(&entry.name),
                description: format!("{display_name}: {}", entry.when_to_use),
                parameters: delegation_tool_parameters(),
            })
        })
        .collect()
}

/// The one argument every delegation tool takes: a self-contained task.
fn delegation_tool_parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "task": {
                "type": "string",
                "description": "What you need from this agent, self-contained."
            }
        },
        "required": ["task"]
    })
}

/// `agents[3]`, plus the entry's trimmed name when it has one.
fn entry_label(index: usize, name: &str) -> String {
    if name.is_empty() {
        format!("agents[{index}]")
    } else {
        format!("agents[{index}] ('{name}')")
    }
}

/// The raw `name` of an entry, for error messages about entries that are not
/// objects or whose name is not a usable string.
fn entry_name(item: &serde_json::Value) -> &str {
    item.get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
}

/// A JSON value's kind, for messages a person reads.
fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// Every refusal a bad `agents` key produces.
fn invalid(detail: String) -> AppError {
    AppError::InvalidInput(format!("Invalid 'agents' config: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(agents: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "agents": agents })
    }

    fn raw_name(item: &serde_json::Value) -> &str {
        item.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
    }

    fn repeated_char(count: usize, ch: &str) -> String {
        ch.repeat(count)
    }

    #[test]
    fn absent_or_null_agents_is_empty() {
        let missing = delegation_entries_from_json(&serde_json::json!({}), "manager").unwrap();
        assert!(missing.is_empty());

        let null =
            delegation_entries_from_json(&serde_json::json!({"agents": null}), "manager").unwrap();
        assert!(null.is_empty());

        let empty =
            delegation_entries_from_json(&config(serde_json::json!([])), "manager").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn parses_valid_entries_trimmed_in_order() {
        let cfg = config(serde_json::json!([
            {"name": "  price-analyst  ", "when_to_use": "  Price elasticity by pack size.  "},
            {"name": "promotion-analyst", "when_to_use": "Whether a promotion paid back."}
        ]));

        let entries = delegation_entries_from_json(&cfg, "manager").expect("two valid entries");

        assert_eq!(
            entries,
            vec![
                DelegationEntry {
                    name: "price-analyst".to_string(),
                    when_to_use: "Price elasticity by pack size.".to_string()
                },
                DelegationEntry {
                    name: "promotion-analyst".to_string(),
                    when_to_use: "Whether a promotion paid back.".to_string()
                }
            ]
        );
    }

    #[test]
    fn rejects_non_array() {
        let err = delegation_entries_from_json(
            &config(serde_json::json!({"name": "price-analyst"})),
            "manager",
        )
        .expect_err("an object is not an array");

        assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
        assert!(err.to_string().contains("agents"), "got: {err}");
    }

    #[test]
    fn rejects_more_than_eight() {
        let many: Vec<serde_json::Value> = (0..9)
            .map(|i| serde_json::json!({"name": format!("analyst-{i}"), "when_to_use": "Use."}))
            .collect();
        let cfg = config(serde_json::Value::Array(many));

        let err = delegation_entries_from_json(&cfg, "manager").expect_err("nine entries");

        assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("analyst-8"),
            "must name the offending entry: {msg}"
        );
        assert!(msg.contains('8'), "must name the limit: {msg}");
    }

    #[test]
    fn rejects_entry_with_unknown_key() {
        let cfg = config(serde_json::json!([
            {"name": "price-analyst", "when_to_use": "Price elasticity.", "when": "typo"}
        ]));

        let err = delegation_entries_from_json(&cfg, "manager").expect_err("unknown key");

        assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(msg.contains("price-analyst"), "must name the entry: {msg}");
        assert!(msg.contains("when"), "must name the unknown key: {msg}");
    }

    #[test]
    fn rejects_missing_or_empty_name() {
        for bad in [
            serde_json::json!({"when_to_use": "Price elasticity."}),
            serde_json::json!({"name": "", "when_to_use": "Price elasticity."}),
            serde_json::json!({"name": "   ", "when_to_use": "Price elasticity."}),
            serde_json::json!({"name": 7, "when_to_use": "Price elasticity."}),
        ] {
            let err = delegation_entries_from_json(
                &config(serde_json::Value::Array(vec![bad])),
                "manager",
            )
            .expect_err("a name is required");
            assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
        }
    }

    #[test]
    fn rejects_when_to_use_empty_or_over_300_chars() {
        for bad in [
            serde_json::json!({"name": "price-analyst", "when_to_use": ""}),
            serde_json::json!({"name": "price-analyst", "when_to_use": "   "}),
            serde_json::json!({"name": "price-analyst"}),
            serde_json::json!({"name": "price-analyst", "when_to_use": repeated_char(301, "é")}),
        ] {
            let err = delegation_entries_from_json(
                &config(serde_json::Value::Array(vec![bad])),
                "manager",
            )
            .expect_err("when_to_use is required, 1-300 chars");
            assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
        }

        // Exactly 300 characters, and multi-byte so bytes differ from chars.
        let ok = delegation_entries_from_json(
            &config(serde_json::Value::Array(vec![serde_json::json!({
                "name": "price-analyst",
                "when_to_use": repeated_char(300, "é")
            })])),
            "manager",
        )
        .expect("exactly 300 characters");
        assert_eq!(ok[0].when_to_use.chars().count(), MAX_WHEN_TO_USE_CHARS);
        assert!(ok[0].when_to_use.len() > MAX_WHEN_TO_USE_CHARS);
    }

    #[test]
    fn rejects_self_reference() {
        let cfg = config(serde_json::Value::Array(vec![serde_json::json!({
            "name": "price-analyst",
            "when_to_use": "Own name."
        })]));

        let err = delegation_entries_from_json(&cfg, "price-analyst")
            .expect_err("an agent may not call itself");

        assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(msg.contains("price-analyst"), "must name the entry: {msg}");
    }

    #[test]
    fn rejects_duplicate_name() {
        let cfg = config(serde_json::Value::Array(vec![
            serde_json::json!({"name": "price-analyst", "when_to_use": "Price."}),
            serde_json::json!({"name": "price-analyst", "when_to_use": "Price again."}),
        ]));

        let err = delegation_entries_from_json(&cfg, "manager").expect_err("duplicate name");

        assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
    }

    #[test]
    fn rejects_tool_name_collision() {
        let cfg = config(serde_json::Value::Array(vec![
            serde_json::json!({"name": "price-analyst", "when_to_use": "Price."}),
            serde_json::json!({"name": "price_analyst", "when_to_use": "Price again."}),
        ]));

        let err = delegation_entries_from_json(&cfg, "manager").expect_err("one tool name");

        assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("agent__price_analyst"),
            "must name the tool: {msg}"
        );
    }

    #[test]
    fn rejects_tool_name_over_64_chars() {
        let long = repeated_char(58, "a");
        let cfg = config(serde_json::Value::Array(vec![serde_json::json!({
            "name": long,
            "when_to_use": "Too long a tool name."
        })]));

        let err = delegation_entries_from_json(&cfg, "manager").expect_err("tool name too long");

        assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
        let msg = err.to_string();
        assert!(msg.contains("64"), "must name the limit: {msg}");

        // 57 characters plus the 7 of the prefix is exactly 64.
        let edge = delegation_entries_from_json(
            &config(serde_json::Value::Array(vec![serde_json::json!({
                "name": repeated_char(57, "a"),
                "when_to_use": "Fits exactly."
            })])),
            "manager",
        )
        .expect("64 characters is the limit, not 63");
        assert_eq!(
            agent_tool_name(&edge[0].name).chars().count(),
            MAX_AGENT_TOOL_NAME_CHARS
        );
    }

    #[test]
    fn agent_tool_name_sanitizes() {
        assert_eq!(agent_tool_name("Price-Analyst"), "agent__price_analyst");
        assert_eq!(agent_tool_name("a.b c"), "agent__a_b_c");
        assert_eq!(agent_tool_name("analyst_1"), "agent__analyst_1");
    }

    #[test]
    fn is_agent_tool_name_checks_prefix() {
        assert!(is_agent_tool_name("agent__price_analyst"));
        assert!(is_agent_tool_name("agent__"));
        assert!(!is_agent_tool_name("builtin_search"));
        assert!(!is_agent_tool_name(""));
    }

    #[test]
    fn tool_definitions_skip_unavailable_and_keep_order() {
        let entries = delegation_entries_from_json(
            &config(serde_json::Value::Array(vec![
                serde_json::json!({"name": "price-analyst", "when_to_use": "Price elasticity."}),
                serde_json::json!({"name": "promotion-analyst", "when_to_use": "Payback."}),
                serde_json::json!({"name": "pack-mix-analyst", "when_to_use": "Pack mix."}),
            ])),
            "manager",
        )
        .expect("three valid entries");

        let display_names = HashMap::from([
            ("price-analyst".to_string(), "Price Analyst".to_string()),
            (
                "pack-mix-analyst".to_string(),
                "Pack-mix Analyst".to_string(),
            ),
        ]);

        let definitions = delegation_tool_definitions(&entries, &display_names);

        assert_eq!(definitions.len(), 2, "missing specialists are skipped");
        assert_eq!(definitions[0].name, "agent__price_analyst");
        assert_eq!(
            definitions[0].description,
            "Price Analyst: Price elasticity."
        );
        assert_eq!(definitions[1].name, "agent__pack_mix_analyst");
        assert_eq!(definitions[1].description, "Pack-mix Analyst: Pack mix.");
        assert_eq!(
            definitions[0].parameters,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "What you need from this agent, self-contained."
                    }
                },
                "required": ["task"]
            })
        );
        assert_eq!(definitions[1].parameters, definitions[0].parameters);
    }
}
