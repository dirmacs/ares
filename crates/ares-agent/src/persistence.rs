//! The tenant agent `persistence_mode` config key (User Memory, step 1).
//!
//! Where an agent holds per-user memory between calls is a per-agent setting
//! with four values: `none` (the default, today's behaviour), `session`,
//! `client_fields` and `eruka_user_graph`. This step only adds the setting
//! and makes it safe: the three non-default modes are not built yet, so a
//! run whose mode is not [`PersistenceMode::None`] is refused where the run
//! is resolved, before any model call (fail closed) — never silently run as
//! if it were `none`.

use std::str::FromStr;

use ares_types::types::{AppError, Result};

/// Every string `persistence_mode` accepts, in documentation order.
pub const PERSISTENCE_MODE_VALUES: [&str; 4] =
    ["none", "session", "client_fields", "eruka_user_graph"];

/// Where an agent keeps per-user memory between calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PersistenceMode {
    /// Keeps nothing about a user between calls (today's behaviour).
    #[default]
    None,
    /// Conversation history kept on our side.
    Session,
    /// The client holds the data and sends it with each request.
    ClientFields,
    /// The data is held in Eruka.
    ErukaUserGraph,
}

impl PersistenceMode {
    /// The exact lowercase string the config key carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Session => "session",
            Self::ClientFields => "client_fields",
            Self::ErukaUserGraph => "eruka_user_graph",
        }
    }
}

impl FromStr for PersistenceMode {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "none" => Ok(Self::None),
            "session" => Ok(Self::Session),
            "client_fields" => Ok(Self::ClientFields),
            "eruka_user_graph" => Ok(Self::ErukaUserGraph),
            _ => Err(AppError::InvalidInput(format!(
                "'persistence_mode' must be one of '{}', was '{s}'",
                PERSISTENCE_MODE_VALUES.join("', '")
            ))),
        }
    }
}

/// Parses the optional `persistence_mode` key of a tenant agent config.
///
/// Absent and `null` both mean the default, [`PersistenceMode::None`]. Any
/// other string than the four listed values, and any non-string, is refused
/// naming the key and the allowed values: a stored row must fail closed,
/// never run as if it were `none`.
pub fn persistence_mode_from_json(config: &serde_json::Value) -> Result<PersistenceMode> {
    match config.get("persistence_mode") {
        None | Some(serde_json::Value::Null) => Ok(PersistenceMode::None),
        Some(serde_json::Value::String(value)) => value.parse(),
        Some(other) => Err(AppError::InvalidInput(format!(
            "'persistence_mode' must be one of '{}', was {}",
            PERSISTENCE_MODE_VALUES.join("', '"),
            json_kind(other)
        ))),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_string_round_trips() {
        for value in PERSISTENCE_MODE_VALUES {
            let mode: PersistenceMode = value.parse().expect("a listed value parses");
            assert_eq!(mode.as_str(), value, "{value} round-trips");
        }
    }

    #[test]
    fn default_is_none() {
        assert_eq!(PersistenceMode::default(), PersistenceMode::None);
        assert_eq!(PersistenceMode::None.as_str(), "none");
    }

    #[test]
    fn from_str_refuses_any_other_string() {
        for bad in ["None", "NONE", "memory", "", " none", "sessions"] {
            let err = bad.parse::<PersistenceMode>().expect_err("unlisted string");
            assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
            let msg = err.to_string();
            assert!(msg.contains("persistence_mode"), "names the key: {msg}");
            for value in PERSISTENCE_MODE_VALUES {
                assert!(msg.contains(value), "lists '{value}': {msg}");
            }
        }
    }

    #[test]
    fn from_json_treats_absent_and_null_as_default() {
        assert_eq!(
            persistence_mode_from_json(&serde_json::json!({"model": "m"})).expect("absent"),
            PersistenceMode::None
        );
        assert_eq!(
            persistence_mode_from_json(
                &serde_json::json!({"model": "m", "persistence_mode": null})
            )
            .expect("null"),
            PersistenceMode::None
        );
    }

    #[test]
    fn from_json_parses_each_listed_value() {
        for value in PERSISTENCE_MODE_VALUES {
            let mode =
                persistence_mode_from_json(&serde_json::json!({"persistence_mode": value}))
                    .expect("a listed value parses");
            assert_eq!(mode.as_str(), value);
        }
    }

    #[test]
    fn from_json_refuses_non_strings() {
        for bad in [
            serde_json::json!(1),
            serde_json::json!(true),
            serde_json::json!([]),
            serde_json::json!({"mode": "none"}),
        ] {
            let err = persistence_mode_from_json(&serde_json::json!({"persistence_mode": bad}))
                .expect_err("persistence_mode must be a string");
            assert!(matches!(err, AppError::InvalidInput(_)), "got {err:?}");
            let msg = err.to_string();
            for value in PERSISTENCE_MODE_VALUES {
                assert!(msg.contains(value), "lists '{value}': {msg}");
            }
        }
    }
}
