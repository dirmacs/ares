#[cfg(test)]
mod tests {
    use super::*;

    fn config(agents: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "agents": agents })
    }

    fn raw_name(item: &serde_json::Value) -> &str {
        item.get("name").and_then(|v| v.as_str()).unwrap_or_default()
    }

    fn repeated_char(count: usize, ch: &str) -> String {
        ch.repeat(count)
    }

    #[test]
    fn absent_or_null_agents_is_empty() {
        let missing = delegation_entries_from_json(&serde_json::json!({}), "manager").unwrap();
        assert!(missing.is_empty());

        let null = delegation_entries_from_json(&serde_json::json!({"agents": null}), "manager")
            .unwrap();
        assert!(null.is_empty());

        let empty = delegation_entries_from_json(&config(serde_json::json!([])), "manager").unwrap();
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
        let err =
            delegation_entries_from_json(&config(serde_json::json!({"name": "price-analyst"})), "manager")
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
        assert!(msg.contains("analyst-8"), "must name the offending entry: {msg}");
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
            let err = delegation_entries_from_json(&config(serde_json::Value::Array(vec![bad])), "manager")
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
            let err = delegation_entries_from_json(&config(serde_json::Value::Array(vec![bad])), "manager")
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
        assert!(msg.contains("agent__price_analyst"), "must name the tool: {msg}");
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
        assert_eq!(agent_tool_name(&edge[0].name).chars().count(), MAX_AGENT_TOOL_NAME_CHARS);
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
            ("pack-mix-analyst".to_string(), "Pack-mix Analyst".to_string()),
        ]);

        let definitions = delegation_tool_definitions(&entries, &display_names);

        assert_eq!(definitions.len(), 2, "missing specialists are skipped");
        assert_eq!(definitions[0].name, "agent__price_analyst");
        assert_eq!(
            definitions[0].description,
            "Price Analyst: Price elasticity."
        );
        assert_eq!(definitions[1].name, "agent__pack_mix_analyst");
        assert_eq!(
            definitions[1].description,
            "Pack-mix Analyst: Pack mix."
        );
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
