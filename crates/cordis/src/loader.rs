//! Declarative loader with config reconciliation (Phase 3).
//!
//! `Loader` reconciles a desired [`EntryTree`] against the current tree and
//! emits per-entry [`LoaderAction`]s.  This replaces the ad-hoc `notify` + `ArcSwap`
//! hot-reload previously scattered across `AresConfigManager`, `DynamicConfigManager`,
//! `RuntimeToolRegistry::start_background_reload`, `ProviderRegistry`, and
//! `NvidiaCatalogCache` (see `docs/cordis-mapping.md` §11).
//! The unified hot-reload path is now `ReflectService::notify(TypeId)` which
//! BFS-walks `dependents: RwLock<HashMap<TypeId, Vec<FiberId>>>` and triggers
//! `Fiber::refresh` via `watch` channels (`notifiers: RwLock<HashMap<TypeId, watch::Sender<()>>>`)
//! — polling via `RuntimeToolRegistry::start_background_reload` 60s `interval` is deprecated:
//! `// REMOVED: polling fallback retained for one release then delete` (see `ReflectService` in `cordis`).
//!
//! Persistence is to `config/entries.json` (JSON) or, when the `toon` feature
//! is enabled, `config/cordis-entries.toon` via `toon-format 0.4.1`.  It never
//! touches `ares.toml` which remains a symlink to `/opt/ares-config/ares.toml`
//! — the loader writes to `config/entries.json` / `config/cordis-entries.toon`
//! separate from `ares.toml`.

use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{CordisError, LoaderJournal, Service};

/// JSON intercept overlay from [`Entry::intercept`], readable via `ctx.get`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryIntercept(pub HashMap<String, serde_json::Value>);

impl Service for EntryIntercept {
    fn name(&self) -> &'static str {
        "entry_intercept"
    }
}

/// TOML wrapper struct for `[[entry]]` array deserialization.
#[derive(Debug, Deserialize, Serialize)]
struct TomlEntries {
    #[serde(default)]
    entry: Vec<Entry>,
}

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
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub isolate: Option<String>,
    #[serde(default)]
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
        let data =
            std::fs::read_to_string(path).map_err(|e| CordisError::Configuration(e.to_string()))?;
        serde_json::from_str(&data).map_err(|e| CordisError::Configuration(e.to_string()))
    }

    /// Serialize the tree to TOML, preserving any leading comment header
    /// (lines starting with '#', plus blank lines) already present in the
    /// existing file. Comments cannot survive serde round-trips, so they
    /// are captured verbatim from the current file content and prepended
    /// to the regenerated body.
    pub fn save_to_toml_file(&self, path: &Path) -> Result<(), CordisError> {
        let mut header = String::new();
        if let Ok(existing) = std::fs::read_to_string(path) {
            for line in existing.lines() {
                if line.starts_with('#') || line.trim().is_empty() {
                    header.push_str(line);
                    header.push('\n');
                } else {
                    break;
                }
            }
        }
        let body = toml::to_string_pretty(&TomlEntries {
            entry: self.0.clone(),
        })
        .map_err(|e| CordisError::Configuration(e.to_string()))?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| CordisError::Configuration(e.to_string()))?;
            }
        }
        std::fs::write(path, format!("{header}{body}"))
            .map_err(|e| CordisError::Configuration(e.to_string()))?;
        Ok(())
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
    RebuildFiber {
        id: String,
        plugin: String,
    },
    UpdateConfig {
        id: String,
        new_config: serde_json::Value,
    },
    Retire {
        id: String,
    },
    Begin {
        id: String,
    },
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

/// Shared, mutable view of the last successfully applied entry tree plus the
/// file it was loaded from. Provided as a Service so the file watcher, admin
/// reload endpoint, and boot all operate on the same state.
#[derive(Clone)]
pub struct CurrentEntries {
    pub tree: std::sync::Arc<std::sync::Mutex<EntryTree>>,
    pub path: std::path::PathBuf,
}

impl Service for CurrentEntries {}

/// Optional boot-time hook letting the loader fill empty entry configs before
/// later entries instantiate. The server binary provides an implementation
/// backed by its Overlay; library users may provide their own or none.
pub trait EntryConfigFiller: Send + Sync {
    fn fill_empty_entry_configs(&self, tree: &mut EntryTree);
}

/// Service wrapper so `Context::get` can resolve the hook.
#[derive(Clone)]
pub struct EntryConfigFillerHandle(pub std::sync::Arc<dyn EntryConfigFiller>);

impl Service for EntryConfigFillerHandle {}

/// Per-action outcome reported by [`Loader::apply`].
#[derive(Debug, Clone)]
pub struct AppliedAction {
    pub id: String,
    /// `"begin" | "update-config" | "retire" | "rebuild-fiber"`
    pub action: &'static str,
    pub status: Result<(), String>,
}

impl Loader {
    /// Reconcile `current` toward `desired`, executing every action for real.
    ///
    /// Unlike [`Loader::execute_action`] (kept for compatibility), this
    /// orchestrator resolves entry payloads from `desired` so `Begin` and
    /// `RebuildFiber` instantiate with the entry's actual config (fixing
    /// the log-only/`Value::Null` behavior), and `Retire` disposes the live
    /// fiber recorded in `journal`.
    ///
    /// Failure policy: per-entry failures are recorded in that action's
    /// status and the batch CONTINUES; on any failure `current` is left
    /// unchanged so a retry re-diffs cleanly. Returns per-action outcomes.
    pub async fn apply(
        ctx: &Arc<crate::Context>,
        current: &mut EntryTree,
        desired: &EntryTree,
        journal: &crate::LoaderJournal,
    ) -> Vec<AppliedAction> {
        let loader = Loader::new();
        let actions = loader.reconcile(current, desired);
        let mut results: Vec<AppliedAction> = Vec::with_capacity(actions.len());
        let mut any_failure = false;

        for action in &actions {
            let outcome: Result<(), String> = match action {
                LoaderAction::Retire { id } => {
                    // Dispose the live fiber (undo effects) before clearing.
                    if let Some(record) = journal.get(id) {
                        if let Some(fid) = record.fiber_id {
                            if let Some(fiber) = ctx
                                .get::<crate::RegistryService>()
                                .and_then(|rs| rs.get_fiber(fid))
                            {
                                fiber.dispose().await;
                            }
                        }
                    }
                    journal.retire(id);
                    tracing::info!(id = %id, "Loader: retired entry");
                    Ok(())
                }
                LoaderAction::UpdateConfig { id, new_config } => {
                    journal.update_config(id, new_config.clone(), None);
                    // Drive Fiber::update when a live fiber is known.
                    let recorded = journal.get(id).and_then(|r| r.fiber_id);
                    if let Some(fiber) = recorded.and_then(|fid| {
                        ctx.get::<crate::RegistryService>()
                            .and_then(|rs| rs.get_fiber(fid))
                    }) {
                        match tokio::runtime::Handle::try_current() {
                            Ok(handle) => {
                                let ctx_ref = ctx.clone();
                                let fiber_ref = fiber.clone();
                                tokio::task::block_in_place(move || {
                                    handle.block_on(async move { fiber_ref.update(&ctx_ref).await })
                                });
                                Ok(())
                            }
                            Err(_) => Err("no tokio runtime for live fiber update".to_string()),
                        }
                    } else {
                        Ok(())
                    }
                }
                LoaderAction::Begin { id } | LoaderAction::RebuildFiber { id, .. } => {
                    let entry = match desired.0.iter().find(|e| &e.id == id) {
                        Some(e) => e.clone(),
                        None => {
                            results.push(AppliedAction {
                                id: id.clone(),
                                action: "begin",
                                status: Err(format!("entry '{id}' not found in desired tree")),
                            });
                            any_failure = true;
                            continue;
                        }
                    };
                    match Self::instantiate_entry(ctx, &entry) {
                        Ok(_fid) => Ok(()),
                        Err(e) => Err(e.to_string()),
                    }
                }
            };
            if outcome.is_err() {
                any_failure = true;
            }
            let kind = match action {
                LoaderAction::Begin { .. } => "begin",
                LoaderAction::UpdateConfig { .. } => "update-config",
                LoaderAction::Retire { .. } => "retire",
                LoaderAction::RebuildFiber { .. } => "rebuild-fiber",
            };
            let id = match action {
                LoaderAction::Begin { id }
                | LoaderAction::UpdateConfig { id, .. }
                | LoaderAction::Retire { id }
                | LoaderAction::RebuildFiber { id, .. } => id.clone(),
            };
            results.push(AppliedAction {
                id,
                action: kind,
                status: outcome,
            });
        }

        if !any_failure {
            *current = desired.clone();
        }
        results
    }
}

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

    /// Load an [`EntryTree`] from a TOML file (`config/cordis-entries.toml`).
    ///
    /// Expected format:
    /// ```toml
    /// [[entry]]
    /// id = "calculator"
    /// plugin = "CalculatorService"
    /// disabled = false
    ///
    /// [entry.config]
    /// ```
    pub fn load_from_file(path: &std::path::Path) -> Result<EntryTree, CordisError> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            CordisError::Configuration(format!("failed to read {}: {}", path.display(), e))
        })?;
        let parsed: TomlEntries = toml::from_str(&content).map_err(|e| {
            CordisError::Configuration(format!("failed to parse {}: {}", path.display(), e))
        })?;
        Ok(EntryTree(parsed.entry))
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
                actions.push(LoaderAction::Retire {
                    id: (*id).to_string(),
                });
            }
        }

        for (id, desired_entry) in &desired_map {
            match curr_map.get(*id) {
                None => {
                    // New id: Begin unless it is already disabled.
                    if !desired_entry.disabled {
                        actions.push(LoaderAction::Begin {
                            id: (*id).to_string(),
                        });
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
                            actions.push(LoaderAction::Retire {
                                id: (*id).to_string(),
                            });
                        } else {
                            actions.push(LoaderAction::Begin {
                                id: (*id).to_string(),
                            });
                        }
                        continue;
                    }
                }
            }
        }

        actions
    }

    /// Execute a reconciliation action against the context.
    ///
    /// `Begin` / `RebuildFiber` require the plugin factory from the
    /// [`crate::PluginRegistry`]; when it is not provided (or no factory is
    /// registered under the entry's `plugin` name) these arms fall back to
    /// log-only. Startup instantiation of new entries goes through
    /// [`Loader::instantiate`] instead, which reports per-entry results.
    ///
    /// The [`crate::LoaderJournal`] (when provided as a `Service`) makes the
    /// `UpdateConfig` and `Retire` arms real: `UpdateConfig` stores the new
    /// config, bumps `generation`, and calls `Fiber::update` when the journal
    /// knows the live fiber id (leaning on [`crate::RegistryService::get_fiber`]
    /// to resolve it); `Retire` clears the record and bumps `generation`.
    /// When the journal is absent both arms stay log-only.
    pub fn execute_action(action: &LoaderAction, ctx: &std::sync::Arc<crate::Context>) {
        let journal = ctx.get::<LoaderJournal>();
        let registry = ctx.get::<crate::PluginRegistry>();
        match action {
            LoaderAction::RebuildFiber { id, plugin } => {
                let Some(registry) = registry else {
                    tracing::warn!(id = %id, plugin = %plugin,
                        "PluginRegistry not provided; loader actions are log-only");
                    return;
                };
                match registry
                    .get(plugin)
                    .ok_or_else(|| {
                        crate::CordisError::Configuration(format!(
                            "no factory registered for plugin '{plugin}'"
                        ))
                    })
                    .and_then(|factory| factory(ctx, &serde_json::Value::Null))
                {
                    Ok(fid) => {
                        if let Some(journal) = &journal {
                            journal.upsert(id, plugin, serde_json::Value::Null, Some(fid));
                        }
                        tracing::info!(id = %id, plugin = %plugin, fiber_id = %fid,
                            "Loader: rebuilt fiber for entry");
                    }
                    Err(e) => {
                        tracing::warn!(id = %id, plugin = %plugin, error = %e, "Loader: rebuild failed");
                    }
                }
            }
            LoaderAction::UpdateConfig { id, new_config } => {
                let Some(journal) = journal else {
                    tracing::info!(id = %id, "Loader: updating fiber config for entry");
                    return;
                };
                // Resolve the live fiber from the journal's recorded id so a
                // config-only change can drive `Fiber::update` (recompute epoch
                // + dependency satisfaction) rather than a full rebuild.
                let recorded = journal.get(id).and_then(|r| r.fiber_id);
                let fiber = if let Some(fid) = recorded {
                    ctx.get::<crate::RegistryService>()
                        .and_then(|rs| rs.get_fiber(fid))
                } else {
                    None
                };
                if let Some(fiber) = fiber {
                    // `Fiber::update` is async; run it inline only when we are
                    // inside a multi-thread tokio runtime (as production
                    // hot-reload is), matching the `block_in_place` pattern used
                    // by the plugin factories. Hosting a current-thread runtime
                    // or no runtime at all leaves the update journal-only so we
                    // never panic on `block_in_place`/`block_on`.
                    match tokio::runtime::Handle::try_current() {
                        Ok(handle)
                            if handle.runtime_flavor()
                                == tokio::runtime::RuntimeFlavor::CurrentThread =>
                        {
                            tracing::info!(id = %id,
                                "Loader: current-thread runtime; fiber config update is journal-only");
                        }
                        Ok(handle) => {
                            tracing::info!(id = %id, "Loader: applying fiber config update (live fiber)");
                            let ctx_ref = ctx.clone();
                            let fiber_ref = fiber.clone();
                            tokio::task::block_in_place(move || {
                                handle.block_on(fiber_ref.update(&ctx_ref))
                            });
                        }
                        Err(_) => {
                            tracing::info!(id = %id,
                                "Loader: no tokio runtime in scope; fiber config update is journal-only");
                        }
                    }
                } else {
                    tracing::info!(id = %id, "Loader: no live fiber for entry; journal-only config update");
                }
                journal.update_config(id, new_config.clone(), recorded);
                tracing::info!(id = %id, config = %new_config, "Loader: updated fiber config for entry");
            }
            LoaderAction::Retire { id } => {
                if let Some(journal) = &journal {
                    if let Some(removed) = journal.retire(id) {
                        tracing::info!(id = %id, plugin = %removed.plugin,
                            "Loader: retired entry (journal record cleared)");
                    } else {
                        tracing::info!(id = %id, "Loader: retiring entry (no journal record)");
                    }
                } else {
                    tracing::info!(id = %id, "Loader: retiring entry");
                }
            }
            LoaderAction::Begin { id } => {
                // `Entry.plugin` is not carried by this action; startup
                // resolves plugin names via `Loader::instantiate` on the
                // desired tree instead.
                tracing::info!(id = %id, "Loader: beginning entry");
            }
        }
    }

    /// Load `path`, fill via optional Overlay-independent hook is caller-side;
    /// diff against `CurrentEntries`-style current tree and apply for real.
    ///
    /// This is the runtime hot-reload primitive shared by the file watcher and
    /// the admin reload endpoint. Returns `None` when the file cannot be read
    /// or parsed; otherwise per-action outcomes (possibly empty).
    pub async fn reload_current(
        ctx: &Arc<crate::Context>,
        path: &std::path::Path,
        current: &mut EntryTree,
        journal: &crate::LoaderJournal,
    ) -> Option<Vec<AppliedAction>> {
        let mut desired = match Self::load_from_file(path) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "Cordis hot-reload: parse failed");
                return None;
            }
        };
        if let Some(handle) = ctx.get::<crate::loader::EntryConfigFillerHandle>() {
            handle.0.fill_empty_entry_configs(&mut desired);
        }
        Some(Self::apply(ctx, current, &desired, journal).await)
    }

    /// Instantiate one entry by plugin name through the [`crate::PluginRegistry`].
    ///
    /// Looks up the factory registered under `plugin_name`, invokes it with
    /// `(ctx, config)` so the plugin lands via `Context::plugin` (single-source
    /// discipline applies), and returns the resulting fiber id. When the
    /// [`crate::LoaderJournal`] is provided, the successful instantiation
    /// records `{plugin, config, fiber_id: Some(fid), generation+1}` so later
    /// `UpdateConfig` / `Retire` actions can resolve the live fiber. Missing
    /// registry or missing factory are `CordisError::Configuration`.
    pub fn instantiate(
        ctx: &Arc<crate::Context>,
        plugin_name: &str,
        config: &serde_json::Value,
        entry_id: &str,
    ) -> Result<crate::FiberId, crate::CordisError> {
        Self::instantiate_entry(
            ctx,
            &Entry {
                id: entry_id.to_string(),
                plugin: plugin_name.to_string(),
                config: config.clone(),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
        )
    }

    /// Instantiate one [`Entry`], applying `isolate` / `intercept` onto `ctx`.
    ///
    /// `intercept` is bound first so the factory can read [`EntryIntercept`].
    /// After the factory provides, newly inserted TypeIds are labeled with
    /// `isolate` so `get_isolated` matches the entry's realm.
    pub fn instantiate_entry(
        ctx: &Arc<crate::Context>,
        entry: &Entry,
    ) -> Result<crate::FiberId, crate::CordisError> {
        if !entry.intercept.is_empty() {
            ctx.bind_intercept(EntryIntercept(entry.intercept.clone()));
        }
        let before: HashSet<TypeId> = ctx.provided_type_ids().into_iter().collect();
        let Some(registry) = ctx.get::<crate::PluginRegistry>() else {
            return Err(crate::CordisError::Configuration(
                "PluginRegistry missing".into(),
            ));
        };
        let Some(factory) = registry.get(&entry.plugin) else {
            return Err(crate::CordisError::Configuration(format!(
                "no factory registered for plugin '{}'",
                entry.plugin
            )));
        };
        // Dedicated registration fiber: every provide the factory performs is
        // owned by this fiber, so `apply`'s Retire can dispose exactly this
        // entry's effects without touching unrelated services.
        let fiber = std::sync::Arc::new(crate::Fiber::new());
        fiber.set_state(crate::FiberState::Loading);
        // RegistryService is optional: when absent (library deployments),
        // effects still land on the dedicated fiber but disposal-by-retire
        // cannot resolve it later.
        let tracked = ctx
            .get::<crate::RegistryService>()
            .map(|rs| rs.track_fiber(fiber.clone()));
        // When RegistryService is absent, mint a placeholder id so the journal
        // record still exists (retire will be journal-only in that mode).
        #[allow(unused_variables)]
        let fid = tracked.unwrap_or_else(|| {
            crate::context::NEXT_FIBER_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst) as u64
        });
        // Mark the registration fiber active before the factory runs so nested
        // provides (e.g. Store → TenantDb) are immediately resolvable; flip to
        // Failed if the factory errors afterwards.
        fiber.set_state(crate::FiberState::Active {
            epoch: entry.id.clone(),
        });
        let outcome = ctx.with_provider_fiber(&fiber, || factory(ctx, &entry.config));
        // The TRACKED fiber id identifies this registration for later
        // retirement; the factory's own return value (often from an inner
        // `ctx.plugin`) is irrelevant to the loader's lifecycle bookkeeping.
        let fid = match outcome {
            Ok(_factory_fid) => tracked.unwrap_or_else(|| {
                crate::context::NEXT_FIBER_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    as u64
            }),
            Err(e) => {
                fiber.set_state(crate::FiberState::Failed {
                    error: Some(e.to_string()),
                });
                return Err(e);
            }
        };
        if let Some(label) = entry.isolate.as_deref() {
            for tid in ctx.provided_type_ids() {
                if !before.contains(&tid) {
                    ctx.bind_isolate(tid, label);
                }
            }
        }
        if let Some(journal) = ctx.get::<LoaderJournal>() {
            journal.upsert(&entry.id, &entry.plugin, entry.config.clone(), Some(fid));
        }
        tracing::info!(entry_id=%entry.id, plugin=%entry.plugin, fiber_id=%fid, "Loader: instantiated plugin");
        Ok(fid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Context;
    use serde_json::json;
    use std::sync::Arc;

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

    #[test]
    fn reconcile_isolate_or_intercept_change_rebuilds_fiber() {
        let cur = EntryTree(vec![Entry {
            id: "a".into(),
            plugin: "Foo".into(),
            config: json!(null),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let des_isolate = EntryTree(vec![Entry {
            id: "a".into(),
            plugin: "Foo".into(),
            config: json!(null),
            disabled: false,
            isolate: Some("tenant:acme".into()),
            intercept: HashMap::new(),
        }]);
        let loader = Loader::new();
        assert!(matches!(
            loader.reconcile(&cur, &des_isolate)[0],
            LoaderAction::RebuildFiber { .. }
        ));

        let mut intercept = HashMap::new();
        intercept.insert("k".into(), json!(1));
        let des_intercept = EntryTree(vec![Entry {
            id: "a".into(),
            plugin: "Foo".into(),
            config: json!(null),
            disabled: false,
            isolate: None,
            intercept,
        }]);
        assert!(matches!(
            loader.reconcile(&cur, &des_intercept)[0],
            LoaderAction::RebuildFiber { .. }
        ));
    }

    #[test]
    fn test_load_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entries.toml");
        std::fs::write(
            &path,
            r#"
[[entry]]
id = "calc"
plugin = "CalculatorService"
disabled = false

[entry.config]

[[entry]]
id = "events"
plugin = "EventsService"
disabled = true

[entry.config]
"#,
        )
        .unwrap();

        let tree = Loader::load_from_file(&path).unwrap();
        assert_eq!(tree.0.len(), 2);
        assert_eq!(tree.0[0].id, "calc");
        assert_eq!(tree.0[0].plugin, "CalculatorService");
        assert!(!tree.0[0].disabled);
        assert_eq!(tree.0[1].id, "events");
        assert!(tree.0[1].disabled);
    }

    #[test]
    fn test_reconcile_from_loaded_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entries.toml");
        std::fs::write(
            &path,
            r#"
[[entry]]
id = "svc1"
plugin = "PluginA"
disabled = false

[entry.config]
"#,
        )
        .unwrap();

        let desired = Loader::load_from_file(&path).unwrap();
        let current = EntryTree(vec![]);
        let loader = Loader::new();
        let actions = loader.reconcile(&current, &desired);
        // New entry should produce a Begin action
        assert!(!actions.is_empty());
        assert!(matches!(actions[0], LoaderAction::Begin { .. }));
    }

    #[test]
    fn loader_journal_upsert_and_get() {
        let journal = LoaderJournal::new();
        assert!(journal.is_empty());
        journal.upsert("svc:alpha", "AlphaService", json!({"v": 1}), Some(7));
        assert_eq!(journal.len(), 1);
        let rec = journal.get("svc:alpha").expect("record present");
        assert_eq!(rec.plugin, "AlphaService");
        assert_eq!(rec.config, json!({"v": 1}));
        assert_eq!(rec.fiber_id, Some(7));
        assert_eq!(rec.generation, 1);
    }

    #[test]
    fn retire_clears_record_and_bumps_generation_tracking() {
        let journal = LoaderJournal::new();
        journal.upsert("svc:beta", "BetaService", json!({"v": 1}), Some(11));

        // Retire removes the record entirely.
        let removed = journal
            .retire("svc:beta")
            .expect("record present before retire");
        assert_eq!(removed.plugin, "BetaService");
        assert!(journal.get("svc:beta").is_none());
        assert!(journal.is_empty());

        // A later upsert for the same id starts a fresh generation, so the
        // previous record is not re-born at its old generation.
        journal.upsert("svc:beta", "BetaService", json!({"v": 2}), Some(12));
        let rec = journal.get("svc:beta").unwrap();
        assert_eq!(rec.fiber_id, Some(12));
        assert_eq!(rec.generation, 1);
    }

    #[test]
    fn update_config_bumps_generation_and_stores_new_config() {
        let journal = LoaderJournal::new();
        journal.upsert("svc:gamma", "GammaService", json!({"v": 1}), Some(21));
        assert_eq!(journal.get("svc:gamma").unwrap().generation, 1);

        let updated = journal
            .update_config("svc:gamma", json!({"v": 2}), None)
            .expect("record exists");
        assert_eq!(updated.config, json!({"v": 2}));
        assert_eq!(updated.generation, 2);

        // Config persisted in the journal.
        let rec = journal.get("svc:gamma").unwrap();
        assert_eq!(rec.config, json!({"v": 2}));
        assert_eq!(rec.generation, 2);
        // fiber_id unchanged when not explicitly updated.
        assert_eq!(rec.fiber_id, Some(21));
    }

    #[test]
    fn update_config_missing_id_is_noop() {
        let journal = LoaderJournal::new();
        assert!(journal
            .update_config("svc:ghost", json!({"v": 1}), None)
            .is_none());
        assert!(journal.is_empty());
    }

    #[test]
    fn execute_action_retire_clears_journal_record() {
        let ctx = Context::new_root();
        let journal = ctx.provide(LoaderJournal::new());
        journal.upsert("svc:delta", "DeltaService", json!({"v": 1}), Some(31));

        Loader::execute_action(
            &LoaderAction::Retire {
                id: "svc:delta".into(),
            },
            &ctx,
        );
        assert!(journal.get("svc:delta").is_none());
        assert!(journal.is_empty());
    }

    #[test]
    fn execute_action_update_config_bumps_generation_without_fiber() {
        let ctx = Context::new_root();
        let journal = ctx.provide(LoaderJournal::new());
        journal.upsert("svc:epsilon", "EpsilonService", json!({"v": 1}), Some(41));

        Loader::execute_action(
            &LoaderAction::UpdateConfig {
                id: "svc:epsilon".into(),
                new_config: json!({"v": 2}),
            },
            &ctx,
        );

        // No RegistryService / live fiber was resolvable, so the update is
        // journal-only, but the record must still advance generation and store
        // the new config.
        let rec = journal.get("svc:epsilon").expect("record retained");
        assert_eq!(rec.config, json!({"v": 2}));
        assert_eq!(rec.generation, 2);
        assert_eq!(rec.fiber_id, Some(41));
    }

    #[test]
    fn execute_action_update_config_without_journal_is_log_only() {
        let ctx = Context::new_root();
        // No registry, no journal — arm must not panic and must stay log-only.
        Loader::execute_action(
            &LoaderAction::UpdateConfig {
                id: "svc:zeta".into(),
                new_config: json!({"v": 2}),
            },
            &ctx,
        );
        assert!(ctx.get::<LoaderJournal>().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn instantiate_writes_journal_record_and_update_reaches_live_fiber() {
        use crate::RegistryService;

        let ctx = Context::new_root();
        ctx.provide(LoaderJournal::new());
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        // A small plugin factory that provides a service via Context::plugin,
        // mirroring the production factory pattern.
        #[derive(Debug)]
        struct Svc;
        impl Service for Svc {}

        plugin_registry.register(
            "SvcFactory",
            Arc::new(|ctx, config| {
                let _ = config;
                let future = ctx.plugin(Svc);
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
            }),
        );

        let fid = Loader::instantiate(&ctx, "SvcFactory", &json!({"v": 1}), "svc:theta")
            .expect("instantiate should succeed");
        assert!(fid > 0);

        let journal = ctx.get::<LoaderJournal>().expect("journal present");
        let rec = journal
            .get("svc:theta")
            .expect("instantiate wrote journal record");
        assert_eq!(rec.plugin, "SvcFactory");
        assert_eq!(rec.config, json!({"v": 1}));
        assert_eq!(rec.fiber_id, Some(fid));
        assert_eq!(rec.generation, 1);

        // UpdateConfig with the live fiber resolves through RegistryService and
        // drives Fiber::update — repeat it against the same ctx.
        Loader::execute_action(
            &LoaderAction::UpdateConfig {
                id: "svc:theta".into(),
                new_config: json!({"v": 2}),
            },
            &ctx,
        );
        let rec = journal.get("svc:theta").expect("record retained");
        assert_eq!(rec.config, json!({"v": 2}));
        assert_eq!(rec.generation, 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn instantiate_entry_applies_isolate_and_intercept() {
        use crate::RegistryService;
        use std::any::TypeId;

        let ctx = Context::new_root();
        ctx.provide(LoaderJournal::new());
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        #[derive(Debug)]
        struct Svc(String);
        impl Service for Svc {}

        plugin_registry.register(
            "SvcFactory",
            Arc::new(|ctx, config| {
                let label = config
                    .get("mark")
                    .and_then(|v| v.as_str())
                    .unwrap_or("none")
                    .to_string();
                let future = ctx.plugin(Svc(label));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
            }),
        );

        let mut intercept = HashMap::new();
        intercept.insert("timeout".into(), json!(5));
        let entry = Entry {
            id: "svc:acme".into(),
            plugin: "SvcFactory".into(),
            config: json!({"mark": "acme"}),
            disabled: false,
            isolate: Some("tenant:acme".into()),
            intercept,
        };
        Loader::instantiate_entry(&ctx, &entry).expect("instantiate_entry");

        assert_eq!(
            ctx.isolate_label(TypeId::of::<Svc>()).as_deref(),
            Some("tenant:acme")
        );
        let isolated = ctx
            .get_isolated::<Svc>("tenant:acme")
            .expect("isolated Svc");
        assert_eq!(isolated.0, "acme");
        assert!(ctx.get::<Svc>().is_some(), "boot get still sees the plugin");
        let overlay = ctx.get::<EntryIntercept>().expect("EntryIntercept bound");
        assert_eq!(overlay.0.get("timeout"), Some(&json!(5)));
    }

    #[allow(dead_code)]
    fn _assert_exports() {
        let _: AppliedAction;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_begins_instantiate_and_journals() {
        use crate::RegistryService;

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        #[derive(Debug)]
        struct SvcA(u64);
        impl Service for SvcA {}
        #[derive(Debug)]
        struct SvcB(u64);
        impl Service for SvcB {}

        plugin_registry.register(
            "FactoryA",
            Arc::new(|ctx, _cfg| {
                let future = ctx.plugin(SvcA(0));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
            }),
        );
        plugin_registry.register(
            "FactoryB",
            Arc::new(|ctx, _cfg| {
                let future = ctx.plugin(SvcB(0));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
            }),
        );

        let desired = EntryTree(vec![
            Entry {
                id: "a:one".into(),
                plugin: "FactoryA".into(),
                config: json!({}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
            Entry {
                id: "b:two".into(),
                plugin: "FactoryB".into(),
                config: json!({}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
        ]);
        let mut current = EntryTree(vec![]);

        let actions = Loader::apply(&ctx, &mut current, &desired, &journal).await;
        assert_eq!(actions.len(), 2);
        assert!(actions
            .iter()
            .all(|a| a.action == "begin" && a.status.is_ok()));
        assert_eq!(current.0.len(), 2);
        assert!(ctx.get::<SvcA>().is_some());
        assert!(ctx.get::<SvcB>().is_some());
        let rec_a = journal.get("a:one").expect("journal has a");
        assert!(rec_a.fiber_id.is_some());

        // Retire `a`, keep `b`.
        let desired2 = EntryTree(vec![desired.0[1].clone()]);
        let actions = Loader::apply(&ctx, &mut current, &desired2, &journal).await;
        assert_eq!(actions[0].action, "retire");
        assert_eq!(actions[0].status, Ok(()));
        assert!(ctx.get::<SvcA>().is_none(), "retired fiber disposed");
        assert!(ctx.get::<SvcB>().is_some(), "kept entry still live");
        assert!(journal.get("a:one").is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_reports_partial_failure_and_keeps_current() {
        use crate::RegistryService;

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        #[derive(Debug)]
        struct Good(u64);
        impl Service for Good {}

        plugin_registry.register(
            "GoodFactory",
            Arc::new(|ctx, _cfg| {
                let future = ctx.plugin(Good(0));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
            }),
        );
        // No factory for "GhostFactory".

        let desired = EntryTree(vec![
            Entry {
                id: "good:one".into(),
                plugin: "GoodFactory".into(),
                config: json!({}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
            Entry {
                id: "ghost:x".into(),
                plugin: "GhostFactory".into(),
                config: json!({}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
        ]);
        let mut current = EntryTree(vec![]);

        let actions = Loader::apply(&ctx, &mut current, &desired, &journal).await;
        let failed = actions
            .iter()
            .find(|a| a.id == "ghost:x")
            .expect("ghost action");
        assert!(
            failed.status.is_err(),
            "unknown factory must fail its action"
        );
        let good = actions
            .iter()
            .find(|a| a.id == "good:one")
            .expect("good action");
        assert_eq!(good.status, Ok(()));
        assert!(
            ctx.get::<Good>().is_some(),
            "good entry instantiated despite sibling failure"
        );
        assert!(
            current.0.is_empty(),
            "current tree must stay unchanged when any action failed"
        );
    }

    #[test]
    fn save_to_toml_file_round_trips_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entries.toml");
        let tree = EntryTree(vec![
            Entry {
                id: "tool:calc".into(),
                plugin: "CalculatorService".into(),
                config: json!({"precision": 2}),
                disabled: true,
                isolate: None,
                intercept: HashMap::new(),
            },
            Entry {
                id: "svc:acme".into(),
                plugin: "PluginA".into(),
                config: json!({"x": 1}),
                disabled: false,
                isolate: Some("acme".into()),
                intercept: HashMap::new(),
            },
        ]);
        tree.save_to_toml_file(&path).unwrap();
        let loaded = Loader::load_from_file(&path).unwrap();
        assert_eq!(tree, loaded);
    }

    #[test]
    fn save_to_toml_file_preserves_comment_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entries.toml");
        std::fs::write(
            &path,
            r#"# Cordis plugin entries loaded at startup.
# Order matters.

[[entry]]
id = "a"
plugin = "Foo"

[entry.config]

[[entry]]
id = "b"
plugin = "Bar"

[entry.config]
"#,
        )
        .unwrap();

        let mut tree = Loader::load_from_file(&path).unwrap();
        assert_eq!(tree.len(), 2);
        tree.0.push(Entry {
            id: "c".into(),
            plugin: "Baz".into(),
            config: json!({}),
            disabled: false,
            isolate: Some("acme".into()),
            intercept: HashMap::new(),
        });
        tree.save_to_toml_file(&path).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        let first_table = raw.find("[[entry]]").expect("serialized body present");
        let header = &raw[..first_table];
        assert!(
            header.contains("# Cordis plugin entries loaded at startup."),
            "first comment line must survive the round-trip"
        );
        assert!(
            header.contains("# Order matters."),
            "second comment line must survive the round-trip"
        );

        let reloaded = Loader::load_from_file(&path).unwrap();
        assert_eq!(reloaded.len(), 3);
        assert_eq!(reloaded, tree);
    }

    #[test]
    fn save_to_toml_file_empty_tree_writes_valid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.toml");
        EntryTree::default().save_to_toml_file(&path).unwrap();
        let loaded = Loader::load_from_file(&path).unwrap();
        assert_eq!(loaded.len(), 0);
        assert!(loaded.is_empty());
    }
}
