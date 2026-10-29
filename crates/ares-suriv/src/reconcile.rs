//! Reconcile: diff two compiled entry lists into loader actions (`reconcile`, Eval.lean).
//!
//! A direct port of the Lean `reconcile`: same three branches in the same
//! order (retires ++ begins ++ updates), same first-match `find?`
//! semantics on the updates branch, same action shapes. The Lean
//! development proves what this function computes:
//! `reconcile_self_stable` (idempotency, needs distinct ids),
//! `reconcile_empty_desired_retires_all`, `reconcile_begin_ids`, and
//! `reconcile_action_warranted` (every emitted action is warranted by
//! one entry's own comparison — Theorem 10).
//!
//! [`dry_run`] renders the diff without applying it: the compile-only /
//! dry-run half of research#4's execution modes. Live apply stays out —
//! it needs a live kernel harness this crate does not own.

use crate::codegen::{Config, Entry};

/// A loader action, mirroring Lean `LoaderAction` (`LoaderAction`, Eval.lean).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoaderAction {
    /// Start a new entry through the loader lifecycle.
    Begin { id: String, plugin: String },
    /// Remove an entry by LIFO disposal.
    Retire(String),
    /// Patch the config of an entry whose plugin is unchanged.
    UpdateConfig { id: String, config: Config },
    /// Tear down and restart an entry whose plugin changed.
    RebuildFiber { id: String, plugin: String },
}

impl LoaderAction {
    /// The entry id this action addresses (Lean `actionId`, Reconcile.lean:51).
    pub fn action_id(&self) -> &str {
        match self {
            LoaderAction::Begin { id, .. }
            | LoaderAction::Retire(id)
            | LoaderAction::UpdateConfig { id, .. }
            | LoaderAction::RebuildFiber { id, .. } => id,
        }
    }
}

/// Diff `current` against `desired` into loader actions.
///
/// Retires: current entries whose id the desired list does not carry.
/// Begins: desired entries whose id the current list does not carry.
/// Updates: for each desired entry found in current (first id match),
/// rebuild when the plugin differs, update when only the config differs,
/// nothing when identical. Concatenation order is retires, begins, updates.
pub fn reconcile(current: &[Entry], desired: &[Entry]) -> Vec<LoaderAction> {
    let current_ids: Vec<&str> = current.iter().map(|e| e.id.as_str()).collect();
    let desired_ids: Vec<&str> = desired.iter().map(|e| e.id.as_str()).collect();
    let mut actions = Vec::new();
    for e in current
        .iter()
        .filter(|e| !desired_ids.contains(&e.id.as_str()))
    {
        actions.push(LoaderAction::Retire(e.id.clone()));
    }
    for d in desired
        .iter()
        .filter(|e| !current_ids.contains(&e.id.as_str()))
    {
        actions.push(LoaderAction::Begin {
            id: d.id.clone(),
            plugin: d.plugin.clone(),
        });
    }
    for d in desired {
        let Some(c) = current.iter().find(|c| c.id == d.id) else {
            continue;
        };
        if c.plugin != d.plugin {
            actions.push(LoaderAction::RebuildFiber {
                id: d.id.clone(),
                plugin: d.plugin.clone(),
            });
        } else if c.config != d.config {
            actions.push(LoaderAction::UpdateConfig {
                id: d.id.clone(),
                config: d.config.clone(),
            });
        }
    }
    actions
}

/// Dry-run: compile `desired_src`, diff against `current` entries, and render
/// the actions one per line (`retire <id>`, `begin <id> <plugin>`,
/// `update <id>`, `rebuild <id> <plugin>`). No kernel contact: the diff is
/// pure. Errors name the compile failure; an empty diff renders empty.
pub fn dry_run(current: &[Entry], desired_src: &str) -> Result<String, String> {
    let compiled = crate::codegen::compile_source(desired_src)?;
    let actions = reconcile(current, &compiled.entries);
    let mut out = String::new();
    for a in &actions {
        match a {
            LoaderAction::Retire(id) => out.push_str(&format!("retire {id}\n")),
            LoaderAction::Begin { id, plugin } => {
                out.push_str(&format!("begin {id} {plugin}\n"));
            }
            LoaderAction::UpdateConfig { id, .. } => out.push_str(&format!("update {id}\n")),
            LoaderAction::RebuildFiber { id, plugin } => {
                out.push_str(&format!("rebuild {id} {plugin}\n"));
            }
        }
    }
    Ok(out)
}
