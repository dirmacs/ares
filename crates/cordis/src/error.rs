//! Structured validation errors for declarative configuration surfaces.
//!
//! Stringly config errors (`CordisError::InvalidConfig(String)`) are easy to
//! log but lossy for API consumers: an admin PATCH that fails a loader
//! pre-flight can only echo prose. [`ValidationIssue`] pairs the human
//! message with the config location it was found at, [`ValidationError`]
//! aggregates them, and [`CordisError::validation`] lifts the aggregate into
//! the existing InvalidConfig error class without changing that class's
//! Display prefix.
//!
//! The loader trial path additionally stashes per-entry failures here
//! ([`stash_trial_validation`] / [`take_trial_validation`]) because
//! `AppliedAction` rows carry plain strings; the HTTP layer consumes the
//! stash to attach a machine-readable `issues` array to otherwise unchanged
//! 4xx bodies.

use std::collections::HashMap;
use std::fmt;
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};

use crate::service::CordisError;

/// One structured validation failure: a human-readable message plus the
/// config location it came from (`["entry-id", "field", "subfield"]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    /// What is wrong, phrased for an operator.
    pub message: String,
    /// Config path of the failure; empty when the whole document is at
    /// fault.
    pub path: Vec<String>,
}

impl ValidationIssue {
    /// An issue with no path yet; chain [`Self::at`] to place it.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            path: Vec::new(),
        }
    }

    /// Builder: attach the config path this issue was found at. Segments
    /// render joined by `.` in Display (`a.b.c`).
    pub fn at<I, S>(mut self, path: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.path = path.into_iter().map(Into::into).collect();
        self
    }
}

impl fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "- {}", self.message)
        } else {
            write!(f, "- {} (at {})", self.message, self.path.join("."))
        }
    }
}

/// Aggregated validation failures from one configuration surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    /// Every failure found, in discovery order.
    pub issues: Vec<ValidationIssue>,
}

impl ValidationError {
    /// Aggregate already-placed issues.
    pub fn new(issues: Vec<ValidationIssue>) -> Self {
        Self { issues }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered = self
            .issues
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        f.write_str(&rendered)
    }
}

impl std::error::Error for ValidationError {}

/// Per-entry stash of the most recent structured validation failures from
/// loader trial pre-flights (`Loader::trial_config_verified`).
///
/// `AppliedAction` rows flatten errors to strings, so the HTTP PATCH surface
/// could not answer with machine-readable issues. Trials record here keyed
/// by entry id; the handler consumes the slot after a failed apply. Slots
/// mirror the LATEST trial outcome: recording a non-validation error clears
/// the entry, and consumption removes it.
static TRIAL_VALIDATIONS: LazyLock<Mutex<HashMap<String, ValidationError>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record the structured issues carried by `err` for `entry_id`, replacing
/// any earlier record; an error without structured issues clears the slot
/// instead, so a stale list is never served for a different failure mode.
pub fn stash_trial_validation(entry_id: &str, err: &CordisError) {
    let mut stash = TRIAL_VALIDATIONS
        .lock()
        .expect("trial validation stash poisoned");
    match err.validation_error() {
        Some(validation) => {
            stash.insert(entry_id.to_string(), validation.clone());
        }
        None => {
            stash.remove(entry_id);
        }
    }
}

/// Consume the stashed issues for `entry_id`, if any.
pub fn take_trial_validation(entry_id: &str) -> Option<ValidationError> {
    TRIAL_VALIDATIONS
        .lock()
        .expect("trial validation stash poisoned")
        .remove(entry_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_issue_display_renders_path() {
        let placed = ValidationIssue::new("missing url").at(["calc", "url"]);
        assert_eq!(placed.to_string(), "- missing url (at calc.url)");

        let deep = ValidationIssue::new("port out of range").at(["a", "b", "c"]);
        assert_eq!(deep.to_string(), "- port out of range (at a.b.c)");

        let bare = ValidationIssue::new("whole document rejected");
        assert_eq!(bare.to_string(), "- whole document rejected");
    }

    #[test]
    fn validation_error_roundtrips_through_cordis_error() {
        let issues = vec![
            ValidationIssue::new("missing url").at(["calc", "url"]),
            ValidationIssue::new("retries must be numeric").at(["llm", "retries"]),
        ];

        let err = CordisError::validation(issues.clone());
        // InvalidConfig error class: Display keeps the established prefix.
        assert!(err.to_string().starts_with("invalid config: "));
        assert!(
            err.to_string().contains("(at calc.url)"),
            "issue text survives the lift: {err}"
        );

        // Structure survives: the accessor hands back the same issue list.
        let roundtripped = err
            .validation_error()
            .expect("validation error exposes issues");
        assert_eq!(roundtripped.issues, issues);

        // Other variants report no structured issues.
        let plain = CordisError::Configuration("not about validation".into());
        assert!(plain.validation_error().is_none());

        // The aggregate serializes for API payloads.
        let json = serde_json::to_value(roundtripped).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "issues": [
                    {"message": "missing url", "path": ["calc", "url"]},
                    {"message": "retries must be numeric", "path": ["llm", "retries"]},
                ]
            })
        );
    }
}
