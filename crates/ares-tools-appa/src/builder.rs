//! The engine-side gate builder, installed into `ares-tools` at startup.
//!
//! Lives here rather than in `ares-tools` so that crate never names an engine
//! type: the git-only OpenAPPA dependencies stay confined to this crate, which
//! is `publish = false`.

use std::sync::Arc;

use ares_tools::gate::{install_gate_builder, ToolGate};
use serde_json::Value;

use crate::appa::AppaGate;

pub fn gate_from_config(config: &Value) -> Result<Option<Arc<dyn ToolGate>>, cordis::CordisError> {
    let refuse = |detail: String| cordis::CordisError::Configuration(format!("appa: {detail}"));
    let section = config.get("appa");
    let path = match section {
        Some(section)
            if section
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false) =>
        {
            section
                .get("config")
                .and_then(Value::as_str)
                .map(std::path::PathBuf::from)
                .ok_or_else(|| {
                    refuse("the appa section is enabled but names no config file".to_string())
                })?
        }
        _ => match std::env::var("ARES_APPA_CONFIG") {
            Ok(path) if !path.is_empty() => std::path::PathBuf::from(path),
            _ => return Ok(None),
        },
    };
    let section = section.cloned().unwrap_or(Value::Null);

    let root = std::fs::read_to_string(&path).map_err(|error| {
        refuse(format!(
            "the policy {} could not be read: {error}",
            path.display()
        ))
    })?;

    // Batteries compose under the root document the way a root file's include
    // composes them: every `*.toml` in the batteries directory, sorted, each
    // under its file stem.
    let mut batteries: Vec<appa_runtime::config::HostedBattery<'_>> = Vec::new();
    let battery_texts: Vec<(String, String)> =
        match section.get("batteries_dir").and_then(Value::as_str) {
            Some(dir) => {
                let mut entries: Vec<_> = std::fs::read_dir(dir)
                    .map_err(|error| {
                        refuse(format!(
                            "the batteries dir {dir} could not be read: {error}"
                        ))
                    })?
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| {
                        entry.path().extension().and_then(|ext| ext.to_str()) == Some("toml")
                    })
                    .map(|entry| {
                        let name = entry
                            .path()
                            .file_stem()
                            .map(|stem| stem.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let text = std::fs::read_to_string(entry.path()).map_err(|error| {
                            refuse(format!("battery {name} could not be read: {error}"))
                        })?;
                        Ok((name, text))
                    })
                    .collect::<Result<_, _>>()?;
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                entries
            }
            None => Vec::new(),
        };
    for (name, text) in &battery_texts {
        batteries.push(appa_runtime::config::HostedBattery {
            name,
            policy: text,
            // The runtime never reads a process variable the host did not
            // grant here; the lookup answers only what is listed.
            token_env: &[],
        });
    }

    let granted: Vec<String> = section
        .get("granted_env")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let lookup = move |var: &str| {
        granted
            .iter()
            .any(|name| name == var)
            .then(|| std::env::var(var).ok())
            .flatten()
    };

    let timeout_ms = section
        .get("consult_timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(2000);
    let max_body_bytes = section
        .get("max_body_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(65_536)
        .min(usize::MAX as u64) as usize;
    let defaults = appa_runtime::config::HostDefaults::new(
        std::time::Duration::from_millis(timeout_ms),
        max_body_bytes,
    );

    let fence_mode = match section.get("fence_mode").and_then(Value::as_str) {
        Some("read_only") => ares_tools::fence::FenceMode::ReadOnly,
        Some("full") => ares_tools::fence::FenceMode::Full,
        _ => ares_tools::fence::FenceMode::WorkspaceWrite,
    };
    let workspace_root = section
        .get("workspace_root")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    let blocklist: Vec<String> = section
        .get("blocklist")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_else(|| vec![".env".to_string(), "id_rsa".to_string()]);
    let fence_policy = ares_tools::fence::FencePolicy::new(fence_mode, workspace_root, blocklist);

    let gate = AppaGate::open(&root, &batteries, defaults, lookup, fence_policy)
        .map_err(|error| refuse(error.to_string()))?;
    Ok(Some(Arc::new(gate) as Arc<dyn ToolGate>))
}

/// Install the OpenAPPA builder so the tools plugin can construct the gate.
///
/// Call once at process start, before the tools entry is loaded.
pub fn install() {
    install_gate_builder(|config| gate_from_config(config));
}
