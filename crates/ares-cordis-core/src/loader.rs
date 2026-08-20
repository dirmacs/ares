//! Declarative loader with config reconciliation (Phase 3).
//!
//! `Loader` reconciles a desired [`EntryTree`] against the current tree and
//! emits per-entry [`LoaderAction`]s.  This replaces the ad-hoc `notify` + `ArcSwap`
//! hot-reload scattered across `AresConfigManager`, `DynamicConfigManager`,
//! `RuntimeToolRegistry::start_background_reload`, `ProviderRegistry`, and
//! `NvidiaCatalogCache` (see `docs/cordis-mapping.md` §11).
//!
//! Persistence is to `config/entries.json` (JSON) or, when the `toon` feature
//! is enabled, `config/cordis-entries.toon` via `toon-format 0.4.1`.  It never
//! touches `ares.toml` which remains a symlink to `/opt/ares-config/ares.toml`
//! — the loader writes to `config/entries.json` / `config/cordis-entries.toon`
//! separate from `ares.toml`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{CordisError, Service};

/// Canonical on-disk location for the declarative entry tree (JSON).
pub const ENTRIES_PATH: &str = "config/entries.json";

/// Alternative on-disk location when `toon-format 0.4.1` is used (`toon` feature).
/// Kept separate from `ares.toml` (which is a symlink to `/opt/ares-config/ares.toml`);
/// the loader never writes to `ares.toml` — see `config/entries.json` vs `ares.toml` invariant.
pub const CORDIS_ENTRIES_TOON_PATH: &str = "config/cordis-entries.toon";

/// A single declarative loader entry.
///
/// Each entry describes one plugin instance: its unique `id`, the `plugin`
/// type label, opaque JSON `config`, and optional spatial modifiers
/// (`isolate` realm label, `intercept` overrides).  `disabled` gates whether
/// the fiber is `Retire`d or `Begin`n.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub plugin: String,
    pub config: serde_json::Value,
    pub disabled: bool,
    pub isolate: Option<String>,
    pub intercept: HashMap<String, serde_json::Value>,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            id: String::new(),
            plugin: String::new(),
            config: serde_json::Value::Null,
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }
    }
}

/// Ordered set of [`Entry`]s — the declarative desired state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryTree(pub Vec<Entry>);

impl EntryTree {
    pub fn new(entries: Vec<Entry>) -> Self {
        Self(entries)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Entry> {
        self.0.iter()
    }

    /// Serialize to pretty JSON (for `config/entries.json`).
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Deserialize from JSON string with round-trip guarantee via `serde_json`.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Persist to `path` (defaults to [`ENTRIES_PATH`]) as JSON.
    /// When the `toon` feature is enabled callers may use [`CORDIS_ENTRIES_TOON_PATH`]
    /// with `toon-format` encoding (see comment in `save_toon`).
    pub fn save_to_file(&self, path: &str) -> Result<(), CordisError> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| CordisError::Configuration(e.to_string()))?;
        std::fs::write(path, json).map_err(|e| CordisError::Configuration(e.to_string()))?;
        Ok(())
    }

    pub fn load_from_file(path: &str) -> Result<Self, CordisError> {
        let data = std::fs::read_to_string(path)
            .map_err(|e| CordisError::Configuration(e.to_string()))?;
        serde_json::from_str(&data).map_err(|e| CordisError::Configuration(e.to_string()))
    }
}

/// Per-entry diff emitted by [`Loader::reconcile`].
///
/// Dispatch per §13:
/// - `id` / `plugin` change → `RebuildFiber`
/// - `config` change → `UpdateConfig`
/// - `disabled` toggle → `Retire` / `Begin`
/// - `isolate` / `intercept` change → `RebuildFiber` (spatial scope change)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoaderAction {
    RebuildFiber { id: String, plugin: String },
    UpdateConfig { id: String, new_config: serde_json::Value },
    Retire { id: String },
    Begin { id: String },
}

/// Declarative loader — diffs `EntryTree`s incrementally.
///
/// Confluence (Thm 73) correctness condition: regardless of entry application
/// order, the quiescent context must equal static assembly of the final
/// `EntryTree`.  `reconcile` is the field-level diff that callers use to
/// drive `Fiber::refresh` / `Fiber::reload` without manual wiring.
///
/// Persisted to [`ENTRIES_PATH`] (`config/entries.json`) or
/// [`CORDIS_ENTRIES_TOON_PATH`] (`config/cordis-entries.toon` via
/// `toon-format 0.4.1` when `toon` feature is enabled).  Never writes
/// `ares.toml`.
#[derive(Debug, Default, Clone)]
pub struct Loader;

impl Service for Loader {}

impl Loader {
    pub fn new() -> Self {
        Self
    }

    /// Canonical persistence path (`config/entries.json`).
    pub fn persist_path() -> &'static str {
        ENTRIES_PATH
    }

    /// Alternative toon persistence path (`config/cordis-entries.toon`).
    pub fn toon_path() -> &'static str {
        CORDIS_ENTRIES_TOON_PATH
    }

    /// Incremental diff `current → desired` producing ordered [`LoaderAction`]s.
    ///
    /// Rules (per-field dispatch):
    /// - missing `id` in `current` → `Begin` (if not disabled)
    /// - `id` in `current` but not `desired` → `Retire`
    /// - `plugin` changed → `RebuildFiber`
    /// - `config` changed → `UpdateConfig`
    /// - `disabled` toggled → `Retire` / `Begin`
    /// - `isolate` or `intercept` changed → `RebuildFiber`
    pub fn reconcile(&self, current: &EntryTree, desired: &EntryTree) -> Vec<LoaderAction> {
        let mut curr_map: HashMap<&str, &Entry> = HashMap::new();
        for e in &current.0 {
            curr_map.insert(e.id.as_str(), e);
        }
        let mut desired_map: HashMap<&str, &Entry> = HashMap::new();
        for e in &desired.0 {
            desired_map.insert(e.id.as_str(), e);
        }

        let mut actions: Vec<LoaderAction> = Vec::new();

        // Retire entries removed from desired (Confluence: withdrawal).
        for id in curr_map.keys() {
            if !desired_map.contains_key(*id) {
                actions.push(LoaderAction::Retire { id: (*id).to_string() });
            }
        }

        for (id, desired_entry) in &desired_map {
            match curr_map.get(*id) {
                None => {
                    // New id: Begin unless it is already disabled.
                    if !desired_entry.disabled {
                        actions.push(LoaderAction::Begin { id: (*id).to_string() });
                    }
                }
                Some(curr_entry) => {
                    // plugin / id change → rebuild (id is key, so plugin diff is the signal)
                    if curr_entry.plugin != desired_entry.plugin {
                        actions.push(LoaderAction::RebuildFiber {
                            id: (*id).to_string(),
                            plugin: desired_entry.plugin.clone(),
                        });
                        continue;
                    }
                    // isolate / intercept spatial change → rebuild
                    if curr_entry.isolate != desired_entry.isolate
                        || curr_entry.intercept != desired_entry.intercept
                    {
                        actions.push(LoaderAction::RebuildFiber {
                            id: (*id).to_string(),
                            plugin: desired_entry.plugin.clone(),
                        });
                        continue;
                    }
                    // config change → update (fiber.update(new_config))
                    if curr_entry.config != desired_entry.config {
                        actions.push(LoaderAction::UpdateConfig {
                            id: (*id).to_string(),
                            new_config: desired_entry.config.clone(),
                        });
                        continue;
                    }
                    // disabled toggle → retire / begin
                    if curr_entry.disabled != desired_entry.disabled {
                        if desired_entry.disabled {
                            actions.push(LoaderAction::Retire { id: (*id).to_string() });
                        } else {
                            actions.push(LoaderAction::Begin { id: (*id).to_string() });
                        }
                        continue;
                    }
                }
            }
        }

        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn entry_json_round_trip() {
        let entry = Entry {
            id: "tool:calc".into(),
            plugin: "CalculatorService".into(),
            config: json!({"precision": 2}),
            disabled: false,
            isolate: Some("tenant:acme".into()),
            intercept: HashMap::new(),
        };
        let s = serde_json::to_string(&entry).unwrap();
        let back: Entry = serde_json::from_str(&s).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn entry_tree_json_round_trip() {
        let tree = EntryTree(vec![
            Entry {
                id: "a".into(),
                plugin: "Foo".into(),
                config: json!({"x": 1}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
            Entry {
                id: "b".into(),
                plugin: "Bar".into(),
                config: json!(null),
                disabled: true,
                isolate: None,
                intercept: HashMap::new(),
            },
        ]);
        let s = serde_json::to_string(&tree).unwrap();
        let back: EntryTree = serde_json::from_str(&s).unwrap();
        assert_eq!(tree, back);
        let pretty = tree.to_json_pretty().unwrap();
        let back2 = EntryTree::from_json(&pretty).unwrap();
        assert_eq!(tree, back2);
    }

    #[test]
    fn reconcile_config_change() {
        let cur = EntryTree(vec![Entry {
            id: "a".into(),
            plugin: "Foo".into(),
            config: json!({"v": 1}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let des = EntryTree(vec![Entry {
            id: "a".into(),
            plugin: "Foo".into(),
            config: json!({"v": 2}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let loader = Loader::new();
        let acts = loader.reconcile(&cur, &des);
        assert_eq!(acts.len(), 1);
        assert!(matches!(acts[0], LoaderAction::UpdateConfig { .. }));
    }

    #[test]
    fn reconcile_disabled_toggle() {
        let cur = EntryTree(vec![Entry {
            id: "a".into(),
            plugin: "Foo".into(),
            config: json!(null),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let des = EntryTree(vec![Entry {
            id: "a".into(),
            plugin: "Foo".into(),
            config: json!(null),
            disabled: true,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let loader = Loader::new();
        assert!(matches!(
            loader.reconcile(&cur, &des)[0],
            LoaderAction::Retire { .. }
        ));
        assert!(matches!(
            loader.reconcile(&des, &cur)[0],
            LoaderAction::Begin { .. }
        ));
    }

    #[test]
    fn reconcile_plugin_change_rebuild() {
        let cur = EntryTree(vec![Entry {
            id: "a".into(),
            plugin: "Foo".into(),
            config: json!(null),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let des = EntryTree(vec![Entry {
            id: "a".into(),
            plugin: "Bar".into(),
            config: json!(null),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let loader = Loader::new();
        assert!(matches!(
            loader.reconcile(&cur, &des)[0],
            LoaderAction::RebuildFiber { .. }
        ));
    }
}
