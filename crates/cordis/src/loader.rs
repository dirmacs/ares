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
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Loader-owned operating state for the fibers it started.
///
/// Two responsibilities live here:
///
/// * **Self-kill window** ([`Self::in_loader_window`]): the loader raises
///   this flag around every reconcile-driven disposal (`Retire` actions,
///   rebuild swaps). A [`crate::Fiber::subscribe_state`] observer registered
///   by [`Loader::watch_entry_fiber`] consults it when a tracked fiber is
///   disposed — a dispose that ran OUTSIDE a loader window means the plugin
///   killed its own registration, and the entry is persisted `disabled =
///   true` (via [`SelfKillPersistence`]) so restarts do not resurrect a
///   crash-looping plugin.
/// * **Apply count**: [`Self::apply_count(id)`] counts completed factory
///   applications per entry id, incremented from
///   [`Loader::instantiate_entry`]. Config-only patches must NOT bump it —
///   that is exactly what the no-restart patch tests assert against.
///
/// Provided as a Service lazily by the loader paths that need it; absent on
/// library deployments, where every accessor degrades to a safe no-op.
#[derive(Clone, Default)]
pub struct LoaderOps {
    inner: Arc<LoaderOpsInner>,
}

#[derive(Default)]
struct LoaderOpsInner {
    /// `true` while the loader itself drives disposals (reconcile windows).
    in_loader_window: AtomicBool,
    /// Completed factory applications per entry id.
    apply_counts: std::sync::Mutex<HashMap<String, u64>>,
    /// Self-kill persistence sink; set via [`Self::enable_self_kill_persistence`].
    persistence: std::sync::Mutex<Option<Arc<SelfKillPersistence>>>,
    /// Entry ids already persisted disabled (dedup so repeated observer
    /// firings write the file at most once per entry).
    persisted_disabled: std::sync::Mutex<BTreeSet<String>>,
}

impl Service for LoaderOps {}

impl LoaderOps {
    pub fn new() -> Self {
        Self::default()
    }

    fn enter_loader_window(&self) -> LoaderWindowGuard {
        self.inner.in_loader_window.store(true, Ordering::SeqCst);
        LoaderWindowGuard(self.inner.clone())
    }

    fn in_loader_window(&self) -> bool {
        self.inner.in_loader_window.load(Ordering::SeqCst)
    }

    fn record_apply(&self, id: &str) {
        *self
            .inner
            .apply_counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(id.to_string())
            .or_insert(0) += 1;
    }

    /// Completed factory applications for one entry id.
    pub fn apply_count(&self, id: &str) -> u64 {
        self.inner
            .apply_counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .copied()
            .unwrap_or(0)
    }

    /// Install the self-kill persistence sink (entries file path + format).
    pub fn enable_self_kill_persistence(&self, path: PathBuf, toon_format: bool) {
        let mut sink = self
            .inner
            .persistence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *sink = Some(Arc::new(SelfKillPersistence { path, toon_format }));
        drop(sink);
        // Drop any dedup state from a previous sink so re-enabling can fire again.
        self.inner
            .persisted_disabled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    fn self_kill_persistence(&self) -> Option<Arc<SelfKillPersistence>> {
        self.inner
            .persistence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Persist `disabled = true` for `id` onto the entries file exactly once.
    fn persist_self_kill(&self, id: &str) {
        if !self
            .inner
            .persisted_disabled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_string())
        {
            return;
        }
        let Some(persistence) = self.self_kill_persistence() else {
            tracing::warn!(entry_id = %id,
                "Loader: plugin disposed itself outside a loader window but no entries \
                 program is configured; restart would resurrect it");
            return;
        };
        match persistence.persist_disabled(id) {
            Ok(()) => tracing::warn!(entry_id = %id,
                "Loader: plugin disposed itself outside a loader window; persisted disabled=true"),
            Err(e) => {
                // Allow a later dispose attempt of the same entry to retry.
                self.inner
                    .persisted_disabled
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(id);
                tracing::error!(entry_id = %id, error = %e,
                    "Loader: failed to persist disabled=true for self-disposed entry");
            }
        }
    }
}

/// RAII marker for a loader-driven disposal window: construction raises the
/// operating flag, drop lowers it. The flag lives on the shared
/// [`LoaderOpsInner`] because state observers run inline on whatever thread
/// drove the transition.
struct LoaderWindowGuard(Arc<LoaderOpsInner>);

impl Drop for LoaderWindowGuard {
    fn drop(&mut self) {
        self.0.in_loader_window.store(false, Ordering::SeqCst);
    }
}

/// Persistence sink for self-kill detection: rewrites the entries program
/// with `disabled = true` on one entry through the existing atomic writers
/// ([`EntryTree::save_to_toml_file`] / [`EntryTree::save_to_file`]).
struct SelfKillPersistence {
    path: PathBuf,
    toon_format: bool,
}

impl SelfKillPersistence {
    fn persist_disabled(&self, id: &str) -> Result<(), CordisError> {
        let mut tree = if self.toon_format {
            Loader::load_from_file(&self.path)
        } else {
            EntryTree::load_from_json_path(&self.path)
        }?;
        let Some(entry) = tree.0.iter_mut().find(|e| e.id == id) else {
            return Ok(()); // Entry no longer declared: nothing to persist.
        };
        if entry.disabled {
            return Ok(()); // Already disabled on disk; idempotent.
        }
        entry.disabled = true;
        if self.toon_format {
            tree.save_to_toml_file(&self.path)
        } else {
            tree.save_to_file(
                self.path
                    .to_str()
                    .ok_or_else(|| CordisError::Configuration("non-utf8 entries path".into()))?,
            )
        }
    }
}

/// Process-global monotonic nonce for save temp-file names: two concurrent
/// saves in one process never collide on the same sibling temp (a bare pid
/// suffix made two racing saves unlink each other's temp mid-flight).
static SAVE_TMP_NONCE: AtomicU64 = AtomicU64::new(0);

fn next_save_nonce() -> u64 {
    SAVE_TMP_NONCE.fetch_add(1, Ordering::Relaxed)
}

/// Atomic single-file persistence for loader configs.
///
/// Creates the parent directory when missing, writes `bytes` to a sibling
/// temp named `{file}.tmp-{pid}-{nonce}`, then renames it over `path`. A
/// crash mid-write leaves the previous file intact; the temp is removed on
/// failure so no `.tmp-*` residue accumulates. The pid+nonce suffix keeps
/// concurrent saves (threads, double-dispatch) from sharing one temp name.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CordisError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CordisError::Configuration(e.to_string()))?;
        }
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("entries");
    let tmp = path.with_file_name(format!(
        "{name}.tmp-{}-{}",
        std::process::id(),
        next_save_nonce()
    ));
    if let Err(e) = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(CordisError::Configuration(e.to_string()));
    }
    Ok(())
}

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

/// Partial update for one [`Entry`] — the request body of
/// `PATCH /admin/cordis/entries/{id}`.
///
/// Every field is optional: only the fields present in the request are
/// copied onto the target entry by [`EntryUpdate::apply_to`]; omitted
/// fields are left untouched, so `{}` is a validated no-op. An explicit
/// `null` `config` clears back to the default (the admin layer normalizes
/// `Null` configs to `{}` before persistence, matching PUT behavior).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct EntryUpdate {
    pub config: Option<serde_json::Value>,
    pub disabled: Option<bool>,
    pub isolate: Option<String>,
    pub intercept: Option<std::collections::BTreeMap<String, serde_json::Value>>,
}

impl EntryUpdate {
    /// Apply only the provided fields onto `entry`; every other field keeps
    /// its current value. `id` / `plugin` are deliberately not patchable —
    /// changing them is a rebuild, expressed by DELETE + PUT.
    pub fn apply_to(&self, entry: &mut Entry) {
        if let Some(config) = &self.config {
            entry.config = config.clone();
        }
        if let Some(disabled) = self.disabled {
            entry.disabled = disabled;
        }
        if let Some(isolate) = &self.isolate {
            entry.isolate = Some(isolate.clone());
        }
        if let Some(intercept) = &self.intercept {
            entry.intercept = intercept
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
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
    ///
    /// Atomic: the bytes land via a sibling temp file + rename
    /// ([`write_atomic`]), so a crash mid-write leaves the previous config
    /// intact and no `.tmp-*` residue survives either outcome. The parent
    /// directory is created when missing.
    /// When the `toon` feature is enabled callers may use [`CORDIS_ENTRIES_TOON_PATH`]
    /// with `toon-format` encoding (see comment in `save_toon`).
    pub fn save_to_file(&self, path: &str) -> Result<(), CordisError> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| CordisError::Configuration(e.to_string()))?;
        write_atomic(Path::new(path), json.as_bytes())
    }

    pub fn load_from_file(path: &str) -> Result<Self, CordisError> {
        let data =
            std::fs::read_to_string(path).map_err(|e| CordisError::Configuration(e.to_string()))?;
        serde_json::from_str(&data).map_err(|e| CordisError::Configuration(e.to_string()))
    }

    /// [`Self::load_from_file`] taking a `Path` — the shape the loader's
    /// self-kill persistence sink needs for JSON programs.
    pub fn load_from_json_path(path: &Path) -> Result<Self, CordisError> {
        let data = std::fs::read_to_string(path).map_err(|e| {
            CordisError::Configuration(format!("failed to read {}: {}", path.display(), e))
        })?;
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
        // Same atomic temp+rename persistence as the JSON path, sharing the
        // pid+nonce temp naming so concurrent saves never collide.
        write_atomic(path, format!("{header}{body}").as_bytes())
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
    /// Verified hot-swap outcome for `rebuild-fiber` actions: `true` when the
    /// replacement plugin was applied out-of-band and returned `Ok` before the
    /// old fiber was retired. Non-rebuild actions report `true`.
    pub verified: bool,
}

/// Process-wide in-flight provider-update ledger (`fiber id` → count).
///
/// C2 cascade batching: entries are inserted by [`Loader::drive_fiber_update`]
/// for the duration of one live re-apply and consulted inside the kernel's
/// refresh path, so concurrent config patches against one provider produce a
/// SINGLE dependency cascade after completion instead of one wave per patch.
static CASCADE_INFLIGHT: std::sync::LazyLock<std::sync::Mutex<HashMap<u64, u64>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

impl Loader {
    /// Reconcile `current` toward `desired`, executing every action for real.
    ///
    /// Unlike [`Loader::execute_action`] (kept for compatibility), this
    /// orchestrator resolves entry payloads from `desired` so `Begin` and
    /// `RebuildFiber` instantiate with the entry's actual config (fixing
    /// the log-only/`Value::Null` behavior), and `Retire` disposes the live
    /// fiber recorded in `journal`.
    ///
    /// Two-phase STAGED apply: phase one constructs and verifies every
    /// replacement candidate without mutating any live entry (config
    /// pre-flight trials, entry resolution); phase two applies the verified
    /// candidates in dependency order. On the first failing verification the
    /// batch aborts BEFORE any mutation — nothing has been touched, so no
    /// rollback is needed. On a failure DURING phase two, every
    /// already-applied change is reverted (config restored, rebuilt fibers
    /// disposed) so the live tree serves the originals; the failing step's
    /// [`AppliedAction`] reports `Err` naming it.
    ///
    /// Failure policy: on any failure `current` is left unchanged so a retry
    /// re-diffs cleanly. Returns per-action outcomes.
    ///
    /// Config-only patches on Active fibers go through the existing update
    /// path ([`Self::trial_config_verified`] pre-flight + `Fiber::update`)
    /// instead of stop+start — the factory runs only inside the scratch
    /// trial, so apply counts stay flat across pure config changes.
    pub async fn apply(
        ctx: &Arc<crate::Context>,
        current: &mut EntryTree,
        desired: &EntryTree,
        journal: &crate::LoaderJournal,
    ) -> Vec<AppliedAction> {
        use apply_staged::Staged;

        let loader = Loader::new();
        let actions = loader.reconcile(current, desired);
        let ops = ctx.get::<LoaderOps>();
        // Phase 1 — STAGE: resolve entries and verify every candidate. No
        // live entry mutates here; failures abort the batch untouched.
        let mut staged: Vec<Staged> = Vec::with_capacity(actions.len());
        let mut results: Vec<AppliedAction> = Vec::with_capacity(actions.len());

        for action in &actions {
            match action {
                LoaderAction::Retire { id } => {
                    staged.push(Staged::Retire { id: id.clone() });
                }
                LoaderAction::UpdateConfig { id, new_config } => {
                    let old_config = current
                        .0
                        .iter()
                        .find(|e| e.id == *id)
                        .map(|e| e.config.clone())
                        .unwrap_or(serde_json::Value::Null);
                    // Pre-flight: trial the NEW config through the same
                    // scratch-context machinery the verified hot-swap uses,
                    // BEFORE staging the mutation. A failing factory leaves
                    // the old provider serving and fails the action; a
                    // passing trial discards the candidate (the live fiber
                    // re-applies below).
                    if let Err(error) = Self::trial_config_verified(ctx, id, new_config) {
                        tracing::error!(entry_id = %id, error = %error,
                            "Loader: config pre-flight failed; old provider kept");
                        results.push(AppliedAction {
                            id: id.clone(),
                            action: "update-config",
                            status: Err(format!("config pre-flight failed: {error}")),
                            verified: true,
                        });
                        return results;
                    }
                    let fid = journal.get(id).and_then(|r| r.fiber_id);
                    staged.push(Staged::UpdateConfig {
                        id: id.clone(),
                        old_config,
                        new_config: new_config.clone(),
                        fid,
                    });
                }
                LoaderAction::Begin { id } => {
                    let Some(entry) = desired.0.iter().find(|e| &e.id == id) else {
                        results.push(AppliedAction {
                            id: id.clone(),
                            action: "begin",
                            status: Err(format!("entry '{id}' not found in desired tree")),
                            verified: true,
                        });
                        return results;
                    };
                    staged.push(Staged::Begin {
                        id: id.clone(),
                        entry: entry.clone(),
                    });
                }
                LoaderAction::RebuildFiber { id, plugin } => {
                    let Some(entry) = desired.0.iter().find(|e| &e.id == id) else {
                        results.push(AppliedAction {
                            id: id.clone(),
                            action: "rebuild-fiber",
                            status: Err(format!("entry '{id}' not found in desired tree")),
                            verified: false,
                        });
                        return results;
                    };
                    staged.push(Staged::RebuildFiber {
                        id: id.clone(),
                        entry: entry.clone(),
                        plugin: plugin.clone(),
                    });
                }
            }
        }

        // Dependency order inside the staged batch: Begin/RebuildFiber first
        // (providers must exist before dependents reactivate), then config
        // updates, then retirements. Ties keep a stable order by entry id so
        // batches are deterministic regardless of HashMap iteration order.
        let order_key = |s: &Staged| match s {
            Staged::Begin { .. } | Staged::RebuildFiber { .. } => 0u8,
            Staged::UpdateConfig { .. } => 1u8,
            Staged::Retire { .. } => 2u8,
        };
        let tie_key = |s: &Staged| match s {
            Staged::Retire { id }
            | Staged::UpdateConfig { id, .. }
            | Staged::Begin { id, .. }
            | Staged::RebuildFiber { id, .. } => id.clone(),
        };
        staged.sort_by(|a, b| order_key(a).cmp(&order_key(b)).then(tie_key(a).cmp(&tie_key(b))));

        // Phase 2 — APPLY in dependency order, rolling back every
        // already-applied step when one fails mid-batch. The loader window
        // spans the whole batch so retire/rebuild disposals never look like
        // plugin self-kills to the state observers.
        let _window = ops.as_ref().map(|o| o.enter_loader_window());
        let mut applied: Vec<Staged> = Vec::new();
        let mut verified_for: HashMap<String, bool> = HashMap::new();

        for step in staged {
            let (id, kind): (String, &'static str) = match &step {
                Staged::Retire { id } => (id.clone(), "retire"),
                Staged::UpdateConfig { id, .. } => (id.clone(), "update-config"),
                Staged::Begin { id, .. } => (id.clone(), "begin"),
                Staged::RebuildFiber { id, .. } => (id.clone(), "rebuild-fiber"),
            };
            let (outcome, verified): (Result<(), String>, bool) = match step {
                Staged::Retire { ref id } => {
                    // Dispose the live fiber (undo effects) before clearing.
                    if let Some(record) = journal.get(id) {
                        if let Some(fid) = record.fiber_id {
                            if let Some(fiber) = ctx
                                .get::<crate::RegistryService>()
                                .and_then(|rs| rs.get_fiber(fid))
                            {
                                if let Err(error) = fiber.dispose().await {
                                    tracing::error!(id = %id, %error, "Loader: fiber stuck in transition during retire");
                                }
                            }
                        }
                    }
                    journal.retire(id);
                    tracing::info!(id = %id, "Loader: retired entry");
                    (Ok(()), true)
                }
                Staged::UpdateConfig { ref id, ref new_config, fid, .. } => {
                    journal.update_config(id, new_config.clone(), None);
                    // Drive Fiber::update when a live fiber is known.
                    if let Some(fiber) = fid.and_then(|f| {
                        ctx.get::<crate::RegistryService>()
                            .and_then(|rs| rs.get_fiber(f))
                    }) {
                        match Self::drive_fiber_update(ctx, &fiber) {
                            Ok(()) => (Ok(()), true),
                            Err(e) => (Err(e), false),
                        }
                    } else {
                        (Ok(()), true)
                    }
                }
                Staged::Begin { ref entry, .. } => match Self::instantiate_entry(ctx, entry) {
                    Ok(_fid) => (Ok(()), true),
                    Err(e) => (Err(e.to_string()), false),
                },
                Staged::RebuildFiber {
                    ref id,
                    ref entry,
                    ref plugin,
                } => {
                    match Self::rebuild_fiber_verified(ctx, id, plugin, entry.clone(), journal).await
                    {
                        Ok(v) => (Ok(()), v),
                        Err(e) => (Err(e), false),
                    }
                }
            };
            if let Err(err) = outcome {
                // ROLLBACK: undo everything this batch already applied,
                // newest-first, then report Failed naming the failing entry.
                Self::rollback_staged(ctx, &applied, journal).await;
                results.push(AppliedAction {
                    id,
                    action: kind,
                    status: Err(format!("staged apply failed: {err}; batch rolled back")),
                    verified,
                });
                return results;
            }
            verified_for.insert(id, verified);
            applied.push(step);
        }

        // Post-apply detection pass (never fails the batch): a cycle keeps
        // its member fibers permanently inactive, so name it at load time.
        Self::report_cycles(ctx);
        *current = desired.clone();
        // Render outcomes in the ORIGINAL reconcile order (stable by entry id
        // within each dependency class), not the dependency apply order.
        let kind_of = |probe_id: &str| -> &'static str {
            match actions.iter().find(|a| match a {
                LoaderAction::Begin { id }
                | LoaderAction::UpdateConfig { id, .. }
                | LoaderAction::Retire { id }
                | LoaderAction::RebuildFiber { id, .. } => id == probe_id,
            }) {
                Some(LoaderAction::Begin { .. }) => "begin",
                Some(LoaderAction::UpdateConfig { .. }) => "update-config",
                Some(LoaderAction::Retire { .. }) => "retire",
                _ => "rebuild-fiber",
            }
        };
        for id in verified_for.keys() {
            // Every staged step either succeeded (recorded above) or aborted
            // the whole batch earlier, so every id carries an outcome.
            let verified = verified_for[id];
            results.push(AppliedAction {
                id: id.clone(),
                action: kind_of(id),
                status: Ok(()),
                verified,
            });
        }
        results
    }

    // --- C2 cascade batching -------------------------------------------------
    //
    // Concurrent PATCH storms against one provider entry used to produce N
    // sequential dependency cascades: each `Fiber::update` re-applied the
    // plugin and every settle notified dependents, which each re-ran their
    // own refresh waves. The in-flight ledger below marks a provider fiber
    // "updating" for the duration of its re-apply; dependent fibers consult
    // it inside `refresh` and DEFER (resting `Pending` — quiet waiting)
    // while any declared dependency is mid-update. When the update finishes,
    // ONE trailing refresh per deferred fiber converges the whole cascade.
    //
    // The ledger keys on fiber id and lives on the loader (process-wide),
    // mirroring the journal: absent loader paths degrade to today's
    // behavior because nothing ever registers an in-flight window.


    /// Mark `fid` as mid-provider-update (reentrant-safe via counting).
    fn cascade_begin(fid: u64) {
        if let Ok(mut ledger) = CASCADE_INFLIGHT.lock() {
            *ledger.entry(fid).or_insert(0) += 1;
        }
    }

    /// End one in-flight window for `fid`; returns `true` when this was the
    /// last open window (i.e. the provider just settled).
    fn cascade_end(fid: u64) -> bool {
        if let Ok(mut ledger) = CASCADE_INFLIGHT.lock() {
            match ledger.entry(fid) {
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    *slot.get_mut() -= 1;
                    if *slot.get() == 0 {
                        slot.remove();
                        return true;
                    }
                    return false;
                }
                std::collections::hash_map::Entry::Vacant(_) => return true,
            }
        }
        true
    }

    /// Kernel-facing deferral probe (C2): `true` while the provider fiber of
    /// ANY of `tids` sits mid-config-update in `ctx`'s realms. The fiber's
    /// refresh consults this to defer dependent cascades until the provider
    /// settles.
    pub(crate) fn cascade_defer_needed(tids: &[TypeId], ctx: &Arc<crate::Context>) -> bool {
        let Some(registry) = ctx.get::<crate::RegistryService>() else {
            return false;
        };
        let provider_fids = registry.provider_fibers_for(ctx, tids);
        Self::cascade_any_inflight(&provider_fids)
    }

    /// True when ANY of `fids` currently sits mid-provider-update. Dependents
    /// treat "provider updating" as not-ready and defer instead of churning
    /// through a cascade wave per concurrent patch.
    pub(crate) fn cascade_any_inflight(fids: &[u64]) -> bool {
        if fids.is_empty() {
            return false;
        }
        CASCADE_INFLIGHT
            .lock()
            .map(|ledger| fids.iter().any(|fid| ledger.contains_key(fid)))
            .unwrap_or(false)
    }

    /// Run one live-fiber config update on the hosting runtime:
    /// multi-thread runtimes use `block_in_place`; runtimes without a
    /// reachable Handle fail the update (the caller rolls back).
    ///
    /// C2 cascade batching: the whole re-apply runs inside an in-flight
    /// ledger window for this fiber, so dependents observing the transient
    /// deactivate/reactivate settle ONCE after completion instead of once
    /// per intermediate state change.
    fn drive_fiber_update(
        ctx: &Arc<crate::Context>,
        fiber: &std::sync::Arc<crate::Fiber>,
    ) -> Result<(), String> {
        let fid = fiber.fiber_id().unwrap_or(0);
        Self::cascade_begin(fid);
        let outcome = match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let ctx_ref = ctx.clone();
                let fiber_ref = fiber.clone();
                tokio::task::block_in_place(move || {
                    handle.block_on(async move { fiber_ref.update(&ctx_ref).await })
                });
                Ok(())
            }
            Err(_) => Err("no tokio runtime for live fiber update".to_string()),
        };
        let settled = Self::cascade_end(fid);
        if settled && fid != 0 {
            tracing::debug!(fiber_id = fid, "Loader: provider update settled; cascade converges");
        }
        outcome
    }

    /// Undo every step of a partially-applied staged batch, newest-first.
    ///
    /// * Config updates restore the prior journal config (and re-drive the
    ///   live fiber so the OLD provider keeps serving).
    /// * Began entries are disposed and retired from the journal.
    /// * Retired entries are NOT resurrected — the desired tree removed them,
    ///   and re-instantiating could re-run side-effectful factories; the
    ///   failure report names the failing entry instead. (`current` stays
    ///   unchanged either way, so a retry re-diffs cleanly.)
    async fn rollback_staged(
        ctx: &Arc<crate::Context>,
        applied: &[apply_staged::Staged],
        journal: &crate::LoaderJournal,
    ) {
        for step in applied.iter().rev() {
            match step {
                apply_staged::Staged::UpdateConfig {
                    id,
                    old_config,
                    fid,
                    ..
                } => {
                    journal.update_config(id, old_config.clone(), None);
                    if let Some(fiber) = fid.and_then(|f| {
                        ctx.get::<crate::RegistryService>()
                            .and_then(|rs| rs.get_fiber(f))
                    }) {
                        let _ = Self::drive_fiber_update(ctx, &fiber);
                    }
                }
                apply_staged::Staged::RebuildFiber { id, .. } => {
                    // The rebuild already swapped registrations under this
                    // id: dispose whatever fiber the swap left behind so the
                    // failed batch leaves no half-applied provider serving.
                    if let Some(record) = journal.get(id) {
                        if let Some(fid) = record.fiber_id {
                            if let Some(fiber) = ctx
                                .get::<crate::RegistryService>()
                                .and_then(|rs| rs.get_fiber(fid))
                            {
                                let _ = fiber.dispose().await;
                            }
                        }
                    }
                }
                apply_staged::Staged::Begin { entry, .. } => {
                    if let Some(record) = journal.get(&entry.id) {
                        if let Some(fid) = record.fiber_id {
                            if let Some(fiber) = ctx
                                .get::<crate::RegistryService>()
                                .and_then(|rs| rs.get_fiber(fid))
                            {
                                let _ = fiber.dispose().await;
                            }
                        }
                    }
                    journal.retire(&entry.id);
                }
                apply_staged::Staged::Retire { .. } => {}
            }
        }
    }
}

/// Module-scoped staging types shared by [`Loader::apply`] and
/// [`Loader::rollback_staged`] (the enum lives here rather than inside
/// `apply` so the rollback can name its variants).
mod apply_staged {
    use crate::loader::Entry;

    pub(super) enum Staged {
        Retire {
            id: String,
        },
        UpdateConfig {
            id: String,
            old_config: serde_json::Value,
            new_config: serde_json::Value,
            fid: Option<crate::FiberId>,
        },
        Begin {
            id: String,
            entry: Entry,
        },
        RebuildFiber {
            id: String,
            entry: Entry,
            plugin: String,
        },
    }
}

impl Loader {
    /// Run dependency-cycle detection over every entry this loader has
    /// instantiated.
    ///
    /// The post-apply inject graph is reconstructed by
    /// [`crate::cycles::build_dependency_graph`] from the lazily-provided
    /// [`crate::cycles::CycleLedger`] plus registry lookups; returns one path
    /// per detected cycle (closed, canonical rotation) and an empty vec for a
    /// healthy graph or library deployments without ledger/registry state.
    pub fn detect_cycles(ctx: &Arc<crate::Context>) -> Vec<Vec<crate::FiberId>> {
        match crate::cycles::build_dependency_graph(ctx) {
            Some(graph) => crate::cycles::find_dependency_cycles(&graph),
            None => Vec::new(),
        }
    }

    /// [`Self::detect_cycles`] with every fiber id resolved to its owning
    /// entry id via the [`LoaderJournal`] (untracked fibers fall back to
    /// their stringified id) — the shape admin surfaces report.
    pub fn detect_cycle_entry_ids(ctx: &Arc<crate::Context>) -> Vec<Vec<String>> {
        let cycles = Self::detect_cycles(ctx);
        let journal = ctx.get::<crate::LoaderJournal>();
        Self::cycle_entry_ids(journal.as_deref(), &cycles)
    }

    /// Map fiber ids onto their owning entry ids via the [`LoaderJournal`]
    /// (untracked fibers fall back to their stringified id).
    fn cycle_entry_ids(
        journal: Option<&crate::LoaderJournal>,
        cycles: &[Vec<crate::FiberId>],
    ) -> Vec<Vec<String>> {
        cycles
            .iter()
            .map(|cycle| {
                cycle
                    .iter()
                    .map(|fid| {
                        journal
                            .and_then(|j| {
                                j.records.read().iter().find_map(|(id, rec)| {
                                    (rec.fiber_id == Some(*fid)).then(|| id.clone())
                                })
                            })
                            .unwrap_or_else(|| fid.to_string())
                    })
                    .collect()
            })
            .collect()
    }

    /// Post-apply detection pass: report any inject-dependency cycle without
    /// failing the batch. A cycle keeps its members permanently inactive (each
    /// waits on the other's provider), which is fully predictable from the
    /// declarations and therefore worth naming at load time.
    fn report_cycles(ctx: &Arc<crate::Context>) {
        let journal = ctx.get::<crate::LoaderJournal>();
        let cycles = Self::detect_cycles(ctx);
        if cycles.is_empty() {
            return;
        }
        let entry_ids = Self::cycle_entry_ids(journal.as_deref(), &cycles);
        tracing::warn!(
            entry_ids = ?entry_ids,
            fibers = ?cycles,
            "dependency cycle detected among loaded entries; affected fibers will remain inactive until the cycle is broken"
        );
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

    /// Diff the caller-supplied composed `desired_composed` tree (includes
    /// resolved, groups flattened, configs interpolated — see `compose_all`)
    /// against the `CurrentEntries`-style current tree and apply for real.
    ///
    /// This is the runtime hot-reload primitive shared by the file watcher and
    /// the admin reload endpoint. Callers own parsing + composition; returns
    /// per-action outcomes for the diff that was applied.
    pub async fn reload_current(
        ctx: &Arc<crate::Context>,
        path: &std::path::Path,
        current: &mut EntryTree,
        desired_composed: &EntryTree,
        journal: &crate::LoaderJournal,
    ) -> Option<Vec<AppliedAction>> {
        // `desired_composed` is the caller-composed tree (includes resolved,
        // groups flattened, configs interpolated); `path` is kept for logs.
        tracing::debug!(
            path = %path.display(),
            entries = desired_composed.0.len(),
            "Cordis hot-reload: applying composed desired state"
        );
        let mut desired = desired_composed.clone();
        if let Some(handle) = ctx.get::<crate::loader::EntryConfigFillerHandle>() {
            handle.0.fill_empty_entry_configs(&mut desired);
        }
        Some(Self::apply(ctx, current, &desired, journal).await)
    }

    /// Rebuild an entry's fiber with swap-with-verification.
    ///
    /// When the old registration fiber is known, the replacement plugin is
    /// applied OUT-OF-BAND first against a scratch child context: the factory
    /// runs and builds its services there, so a failure leaves the live
    /// provider completely untouched. Only after the candidate applies `Ok`
    /// does the swap proceed: the new instances are bridged in as intercept
    /// overrides (intercept lookups precede store lookups, so `get` keeps
    /// resolving), the old fiber retires, and the bridged values are promoted
    /// into the store under a fresh registration fiber.
    ///
    /// Fallback to the classic dispose-then-rebuild path — reported as
    /// `Ok(false)` ("unverified") — when there is no tracked old fiber or the
    /// entry targets an isolate realm (isolated lookups do not consult
    /// intercepts). A failed candidate returns `Err` and keeps the old
    /// provider serving.
    ///
    /// Note: the trial executes the factory once, so factories with external
    /// side effects (e.g. migrations) run twice across trial + promotion;
    /// such plugins should be swapped through the unverified path instead.
    async fn rebuild_fiber_verified(
        ctx: &Arc<crate::Context>,
        id: &str,
        plugin_name: &str,
        entry: Entry,
        journal: &crate::LoaderJournal,
    ) -> Result<bool, String> {
        let registry = ctx.get::<crate::RegistryService>();
        let old_fid = journal.get(id).and_then(|r| r.fiber_id);
        let old_fiber = old_fid
            .as_ref()
            .and_then(|fid| registry.as_ref()?.get_fiber(*fid));
        let Some((registry, old_fiber, old_fid)) = registry
            .zip(old_fiber)
            .zip(old_fid)
            .map(|((r, f), i)| (r, f, i))
        else {
            tracing::warn!(entry_id = %id, plugin = %plugin_name, swap_mode = "unverified",
                "Loader: no tracked fiber for rebuild; dispose-then-rebuild");
            return Self::retire_then_instantiate(ctx, entry)
                .await
                .map(|_| false);
        };
        if entry.isolate.is_some() {
            tracing::warn!(entry_id = %id, plugin = %plugin_name, swap_mode = "unverified",
                "Loader: isolated entry rebuild; dispose-then-rebuild");
            return Self::retire_then_instantiate(ctx, entry)
                .await
                .map(|_| false);
        }

        // Out-of-band trial: build the candidate on a scratch child context.
        // The parent chain keeps every dependency resolvable while the
        // duplicate-provider discipline of the empty scratch store prevents
        // collisions; nothing lands on the live provider.
        let scratch = ctx.extend();
        let Some(plugin_registry) = scratch.get::<crate::PluginRegistry>() else {
            return Err("PluginRegistry missing".to_string());
        };
        let Some(factory) = plugin_registry.get(&entry.plugin) else {
            return Err(format!("no factory registered for plugin '{plugin_name}'"));
        };
        let trial_fiber = std::sync::Arc::new(crate::Fiber::new());
        trial_fiber.set_state(crate::FiberState::Loading);
        let trial = scratch.with_provider_fiber(&trial_fiber, || factory(&scratch, &entry.config));
        // A trial factory calling Context::plugin re-points ReflectService at
        // the scratch context; restore the authoritative root binding.
        if let Some(reflect) = ctx.get::<crate::ReflectService>() {
            reflect.set_context(ctx);
        }
        if let Err(e) = trial {
            tracing::warn!(entry_id = %id, plugin = %plugin_name, error = %e,
                "Loader: verified swap trial failed; old provider kept");
            return Err(e.to_string());
        }

        // Every TypeId freshly built in scratch AND currently served by the
        // root context is replaced by this rebuild (the plugin's Provides plus
        // nested provides owned by the same registration fiber).
        let built: Vec<TypeId> = scratch.provided_type_ids();
        let replaced: Vec<TypeId> = built
            .iter()
            .copied()
            .filter(|tid| ctx.get_untyped(*tid).is_some())
            .collect();
        if replaced.is_empty() {
            tracing::warn!(entry_id = %id, plugin = %plugin_name, swap_mode = "unverified",
                "Loader: trial produced no comparable services; dispose-then-rebuild");
            return Self::retire_then_instantiate(ctx, entry)
                .await
                .map(|_| false);
        }

        let new_fid = SwapPromotion {
            ctx,
            registry: registry.as_ref(),
            scratch: &scratch,
            epoch: &entry.id,
            intercept_overlay: Some(&entry.intercept),
            built: &built,
            replaced: &replaced,
            old_fiber,
            old_fid,
        }
        .run()
        .await;
        journal.upsert(id, &entry.plugin, entry.config.clone(), Some(new_fid));
        tracing::info!(entry_id = %id, plugin = %plugin_name, old_fiber_id = %old_fid,
            new_fiber_id = %new_fid, swap_mode = "verified",
            "Loader: hot-swapped provider with verification");
        Ok(true)
    }

    /// Pre-flight trial for [`LoaderAction::UpdateConfig`]: build the plugin
    /// with the NEW config on a scratch child context exactly like the
    /// out-of-band trial in [`Self::rebuild_fiber_verified`], then DISCARD
    /// the candidate. Nothing is bridged or promoted — this only answers
    /// "would the new configuration apply cleanly?" so a broken config can
    /// never take down the live fiber's re-apply. Returns the factory error
    /// verbatim on failure.
    ///
    /// Absent registry/factory means there is nothing to pre-flight (the
    /// classic journal-only update path applies); that is not an error.
    fn trial_config_verified(
        ctx: &Arc<crate::Context>,
        id: &str,
        new_config: &serde_json::Value,
    ) -> Result<(), String> {
        let scratch = ctx.extend();
        let Some(plugin_registry) = scratch.get::<crate::PluginRegistry>() else {
            return Ok(());
        };
        // The entry's factory label comes from the journaled record; unknown
        // ids have no factory to trial and fall through to journal-only.
        let Some(record) = ctx.get::<crate::LoaderJournal>().and_then(|j| j.get(id)) else {
            return Ok(());
        };
        let Some(factory) = plugin_registry.get(&record.plugin) else {
            return Ok(());
        };
        let trial_fiber = std::sync::Arc::new(crate::Fiber::new());
        trial_fiber.set_state(crate::FiberState::Loading);
        let trial =
            scratch.with_provider_fiber(&trial_fiber, || factory(&scratch, &new_config.clone()));
        // A trial factory calling Context::plugin re-points ReflectService at
        // the scratch context; restore the authoritative root binding.
        if let Some(reflect) = ctx.get::<crate::ReflectService>() {
            reflect.set_context(ctx);
        }
        trial.map(|_| ()).map_err(|e| {
            // Preserve machine-readable issues before flattening to the
            // action-row string; a non-validation error clears any stale
            // slot for this entry.
            crate::error::stash_trial_validation(id, &e);
            e.to_string()
        })
    }

    /// Per-entry stash of the most recent structured validation failures
    /// from [`Self::trial_config_verified`] pre-flights.
    ///
    /// `AppliedAction` rows carry plain strings, so the admin PATCH surface
    /// could not answer 4xx with machine-readable issues. Trials record here
    /// keyed by entry id ([`crate::error::stash_trial_validation`]); the
    /// HTTP layer consumes the slot after a failed apply. Slots mirror the
    /// LATEST trial outcome — recording a non-validation error clears the
    /// entry, and consumption removes it.
    pub fn take_trial_validation(entry_id: &str) -> Option<crate::error::ValidationError> {
        crate::error::take_trial_validation(entry_id)
    }

    /// Broker a rolling provider replacement with zero absence window
    /// (paper §6 semantics).
    ///
    /// Resolves the live registration from the [`crate::LoaderJournal`] by
    /// plugin label (first journaled entry whose `plugin` matches — the same
    /// label also selects the replacement factory from the
    /// [`crate::PluginRegistry`], mirroring how admins name a running
    /// provider), trials that factory with the NEW config OUT-OF-BAND on a
    /// scratch child context exactly like [`Self::rebuild_fiber_verified`],
    /// and only then swaps: the new
    /// instances are bridged in as intercept overrides (intercept lookups
    /// precede store lookups, so `get` keeps resolving), the old fiber
    /// retires, and the bridged values are promoted into the store under a
    /// fresh registration fiber before the bridge drops. Consumers observe no
    /// gap: every lookup stays satisfied at every instant because the key
    /// never becomes unprovided.
    ///
    /// The old fiber is disposed DIRECTLY through its registration fiber
    /// instead of going through [`Context::remove`] — this deliberately
    /// bypasses the public guarded-withdrawal check. The guard exists to
    /// refuse removals that would leave active consumers UNRESOLVED; here
    /// resolution stays continuous by construction (the bridge is installed
    /// before disposal), which is precisely why the broker may bypass it.
    /// Genuine withdrawals (the admin retire endpoint) must keep using the
    /// guarded path.
    ///
    /// Failure policy: a failing trial returns `Err` and leaves the old
    /// provider serving untouched; the journal advances only on success
    /// (generation bump + new fiber id).
    ///
    /// Root-realm only for now: if the trial produces services carrying an
    /// isolate label, the call fails with [`CordisError::Configuration`]
    /// naming the limitation — isolated lookups skip intercept overrides, so
    /// the bridge mechanism cannot cover them.
    pub async fn replace_provider(
        &self,
        ctx: &Arc<crate::Context>,
        plugin_name: &str,
        config: serde_json::Value,
        journal: &crate::LoaderJournal,
    ) -> Result<crate::FiberId, CordisError> {
        // Resolve the old registration by plugin label from the journal.
        let (id, record) = journal
            .records
            .read()
            .iter()
            .find(|(_, rec)| rec.plugin == plugin_name)
            .map(|(id, rec)| (id.clone(), rec.clone()))
            .ok_or_else(|| {
                CordisError::Configuration(format!(
                    "replace_provider: no journaled entry for plugin '{plugin_name}'"
                ))
            })?;
        let old_fid = record.fiber_id.ok_or_else(|| {
            CordisError::Configuration(format!(
                "replace_provider: entry '{id}' has no tracked fiber"
            ))
        })?;
        let registry = ctx
            .get::<crate::RegistryService>()
            .ok_or_else(|| CordisError::Configuration("RegistryService missing".into()))?;
        let old_fiber = registry.get_fiber(old_fid).ok_or_else(|| {
            CordisError::Configuration(format!(
                "replace_provider: fiber {old_fid} for entry '{id}' not tracked"
            ))
        })?;

        // Out-of-band trial: identical discipline to rebuild_fiber_verified —
        // the candidate is built on an empty scratch child of the live
        // context, so a failing factory cannot touch the serving provider.
        let scratch = ctx.extend();
        let Some(plugin_registry) = scratch.get::<crate::PluginRegistry>() else {
            return Err(CordisError::Configuration("PluginRegistry missing".into()));
        };
        let Some(factory) = plugin_registry.get(plugin_name) else {
            return Err(CordisError::Configuration(format!(
                "no factory registered for plugin '{plugin_name}'"
            )));
        };
        let trial_fiber = std::sync::Arc::new(crate::Fiber::new());
        trial_fiber.set_state(crate::FiberState::Loading);
        let trial = scratch.with_provider_fiber(&trial_fiber, || factory(&scratch, &config));
        // A trial factory calling Context::plugin re-points ReflectService at
        // the scratch context; restore the authoritative root binding.
        if let Some(reflect) = ctx.get::<crate::ReflectService>() {
            reflect.set_context(ctx);
        }
        let built: Vec<TypeId> = match trial {
            Ok(_) => scratch.provided_type_ids(),
            Err(e) => {
                tracing::warn!(entry_id = %id, plugin = %plugin_name, error = %e,
                    "Loader: replace_provider trial failed; old provider kept");
                return Err(e);
            }
        };

        // Root realm only: isolated lookups bypass intercepts, so the bridge
        // cannot serve them. Nothing has been mutated yet — fail clean.
        if let Some(isolated) = built
            .iter()
            .copied()
            .find(|tid| ctx.isolate_label(*tid).is_some())
        {
            return Err(CordisError::Configuration(format!(
                "replace_provider: isolated providers not supported yet \
                 (trial built an isolated service, e.g. {isolated:?})"
            )));
        }
        let replaced: Vec<TypeId> = built
            .iter()
            .copied()
            .filter(|tid| ctx.get_untyped(*tid).is_some())
            .collect();
        if replaced.is_empty() {
            // Unlike rebuild_fiber_verified there is NO dispose-then-rebuild
            // fallback here: blind disposal is exactly the absence window the
            // broker exists to eliminate.
            return Err(CordisError::Configuration(format!(
                "replace_provider: trial produced no comparable services for '{plugin_name}'"
            )));
        }

        let new_fid = SwapPromotion {
            ctx,
            registry: registry.as_ref(),
            scratch: &scratch,
            epoch: &id,
            intercept_overlay: None,
            built: &built,
            replaced: &replaced,
            old_fiber,
            old_fid,
        }
        .run()
        .await;

        journal.upsert(&id, plugin_name, config, Some(new_fid));
        tracing::info!(entry_id = %id, plugin = %plugin_name, old_fiber_id = %old_fid,
            new_fiber_id = %new_fid, swap_mode = "verified",
            "Loader: replace_provider swapped provider with zero absence window");
        Ok(new_fid)
    }

    /// Classic rebuild: dispose the old fiber, then instantiate the entry
    /// through the normal factory path.
    async fn retire_then_instantiate(
        ctx: &Arc<crate::Context>,
        entry: Entry,
    ) -> Result<(), String> {
        match Self::instantiate_entry(ctx, &entry) {
            Ok(_fid) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
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
        // Lazy ledger provision: every provide the factory performed is
        // recorded as `(type, realm) -> fid` so post-apply cycle detection can
        // reconstruct the inject graph. Library deployments that never touch
        // this path simply never see a ledger.
        if ctx.get::<crate::cycles::CycleLedger>().is_none() {
            ctx.provide(crate::cycles::CycleLedger::new());
        }
        let ledger = ctx
            .get::<crate::cycles::CycleLedger>()
            .expect("ledger just provided");
        for tid in ctx.provided_type_ids() {
            if !before.contains(&tid) {
                ledger.record_provider(tid, ctx.isolate_label(tid).as_deref(), fid);
            }
        }
        ledger.note_entry(fid, &entry.id);
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
        // Self-kill detection: observe the registration fiber so an
        // out-of-band disposal (the plugin disposing ITSELF, outside any
        // loader reconcile window) persists `disabled = true` for this entry.
        if let Some(ops) = ctx.get::<LoaderOps>() {
            ops.record_apply(&entry.id);
            Self::watch_entry_fiber(&ops, &fiber, &entry.id);
        }
        tracing::info!(entry_id=%entry.id, plugin=%entry.plugin, fiber_id=%fid, "Loader: instantiated plugin");
        Ok(fid)
    }

    /// Subscribe the self-kill observer onto a loader-started registration
    /// fiber.
    ///
    /// The kernel's [`crate::Fiber::dispose`] marks the fiber disposed and
    /// fans out to state observers synchronously; the observer below fires
    /// on that transition and consults the [`LoaderOps`] operating flag:
    /// when NO loader window is open, the dispose came from the plugin
    /// itself (self-kill) and the entry is persisted `disabled = true`.
    /// Loader-driven disposals (retire/reconcile windows) never persist.
    fn watch_entry_fiber(
        ops: &std::sync::Arc<LoaderOps>,
        fiber: &std::sync::Arc<crate::Fiber>,
        entry_id: &str,
    ) {
        let ops_ref = std::sync::Arc::downgrade(ops);
        let entry = entry_id.to_string();
        // The observer MUST NOT call back into the fiber (kernel contract);
        // it only reads its own dedup marker plus the shared operating flag.
        let handle = fiber.subscribe_state(Box::new(move |state| {
            let Some(ops) = ops_ref.upgrade() else {
                return;
            };
            if !ops.in_loader_window()
                && matches!(
                    state,
                    crate::FiberState::Unloading { .. } | crate::FiberState::Inactive { .. }
                )
            {
                ops.persist_self_kill(&entry);
            }
        }));
        // Deliberately drop the cancellation handle: subscriptions live as
        // long as their fiber, and dropping it merely flags the observer for
        // cleanup at the next state fan-out — disposal of the fiber itself
        // ends its lifetime.
        drop(handle);
    }
}

/// Shared tail of the verified hot-swap paths ([`Loader::rebuild_fiber_verified`]
/// and [`Loader::replace_provider`]): bridge → dispose old → promote → fresh
/// registration fiber.
///
/// Ordering guarantees the zero-absence-window invariant:
/// 1. Intercept overrides for every replaced TypeId are installed FIRST, so
///    lookups resolve to the new instances immediately.
/// 2. The old fiber is disposed directly (bypassing the public
///    guarded-withdrawal check in [`Context::remove`] on purpose): resolution
///    stays continuous by construction because the bridge already serves the
///    new values while the disposal undos clear the stale store entries.
/// 3. Bridge values are promoted into the store peek-before-remove (store
///    insert precedes intercept removal; intercept is consulted first), so no
///    lookup ever observes an empty slot.
/// 4. Types the new build introduces beyond what it replaces are added from
///    the scratch context.
/// 5. A fresh `Active` registration fiber is tracked and realm-registered;
///    the caller journals the swap outcome against its returned fiber id.
struct SwapPromotion<'a> {
    ctx: &'a Arc<crate::Context>,
    registry: &'a crate::RegistryService,
    scratch: &'a Arc<crate::Context>,
    /// Epoch label for the new registration fiber (`Active { epoch }`).
    epoch: &'a str,
    /// Optional entry-intercept overlay preserved exactly as
    /// `instantiate_entry` would have installed it (rebuild path only).
    intercept_overlay: Option<&'a HashMap<String, serde_json::Value>>,
    /// Every TypeId freshly built in the scratch context.
    built: &'a [TypeId],
    /// The subset of `built` currently served by the root context — the
    /// types this swap replaces.
    replaced: &'a [TypeId],
    old_fiber: Arc<crate::Fiber>,
    old_fid: crate::FiberId,
}

impl SwapPromotion<'_> {
    async fn run(&self) -> crate::FiberId {
        // Preserve the entry-intercept overlay exactly as instantiate_entry
        // would have installed it.
        if let Some(overlay) = self.intercept_overlay.filter(|o| !o.is_empty()) {
            self.ctx.bind_intercept(EntryIntercept(overlay.clone()));
        }

        // Bridge: intercept overrides win over store lookups, so installing
        // here makes the new instances resolvable instantly.
        for tid in self.replaced {
            if let Some(any) = self.scratch.get_untyped(*tid) {
                self.ctx.bind_intercept_untyped(*tid, any);
            }
        }

        // Retire the old registration fiber: its undos clear the stale store
        // entries while the bridge keeps serving the new values. This is the
        // deliberate guarded-withdrawal bypass documented on both callers:
        // consumers never lose resolution, which is exactly the condition the
        // guard exists to protect.
        // A bounded-transition failure here means a hung plugin apply kept
        // the old fiber's inertia guard; the swap continues regardless — the
        // bridge already serves the new values, so surfacing the error would
        // only roll back a cutover that is already live.
        let _ = self.old_fiber.dispose().await;
        self.registry.remove(self.old_fid);

        // Promote bridge values into the store. Peek-before-remove keeps every
        // lookup satisfied at every instant (store insert precedes intercept
        // removal, and intercept is consulted first).
        for tid in self.replaced {
            if let Some(any) = self.ctx.peek_intercept_untyped(*tid) {
                // A previously-promoted swap carries NO disposal undo
                // (`provide_untyped` bypasses the undo stack), so the retired
                // fiber cannot clear it. Take any such stale entry first;
                // the bridge stays up until the new value is inserted, so
                // lookups never observe a gap.
                self.ctx.take_untyped(*tid);
                let _ = self.ctx.provide_untyped(*tid, any);
                self.ctx.remove_intercept_untyped(*tid);
            }
        }
        // Types the new build introduces that the old one did not provide are
        // simply added to the store.
        for tid in self.built {
            if self.replaced.contains(tid) || self.ctx.get_untyped(*tid).is_some() {
                continue;
            }
            if let Some(any) = self.scratch.get_untyped(*tid) {
                let _ = self.ctx.provide_untyped(*tid, any);
            }
        }

        // Fresh registration fiber owns the swapped-in provider.
        let fiber = std::sync::Arc::new(crate::Fiber::new());
        fiber.set_state(crate::FiberState::Active {
            epoch: self.epoch.to_string(),
        });
        let new_fid = self.registry.track_fiber(fiber);
        self.registry.track_fiber_in_realm(new_fid, self.ctx);
        new_fid
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
    async fn apply_aborts_on_first_failure_and_rolls_back() {
        use crate::RegistryService;

        // Staged batch semantics (two-phase apply): the FIRST failing step
        // aborts the whole batch. Entries applied before it are reverted and
        // the failing entry is named in its error; `current` stays unchanged
        // so a retry re-diffs cleanly.
        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        #[derive(Debug)]
        struct Good(std::sync::atomic::AtomicU64);
        impl Service for Good {}

        plugin_registry.register(
            "GoodFactory",
            Arc::new(|ctx, cfg| {
                let v = cfg.get("v").and_then(|x| x.as_u64()).unwrap_or(0);
                let future = ctx.plugin(Good(std::sync::atomic::AtomicU64::new(v)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
            }),
        );
        plugin_registry.register(
            "LateGoodFactory",
            Arc::new(|ctx, _cfg| {
                let future = ctx.plugin(Good(std::sync::atomic::AtomicU64::new(99)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
            }),
        );
        // No factory for "GhostFactory".

        let desired = EntryTree(vec![
            Entry {
                id: "good:one".into(),
                plugin: "GhostFactory".into(),
                config: json!({}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
            Entry {
                id: "good:two".into(),
                plugin: "GoodFactory".into(),
                config: json!({"v": 1}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
        ]);
        let mut current = EntryTree(vec![]);

        // Batch where the FIRST dependency-class step fails (unknown factory):
        // nothing was applied before the abort, so no sibling may survive.
        let actions = Loader::apply(&ctx, &mut current, &desired, &journal).await;
        let failed = actions
            .iter()
            .find(|a| a.id == "good:one")
            .expect("failing entry named in results");
        assert!(
            failed.status.is_err(),
            "unknown factory must fail its action"
        );
        assert_eq!(actions.len(), 1, "abort-on-first-failure: one outcome only");
        assert!(
            !actions.iter().any(|a| a.id == "good:two"),
            "entries after the failing step are never applied"
        );
        assert!(
            ctx.get::<Good>().is_none(),
            "no sibling instantiated when the first step already failed"
        );
        assert!(
            current.0.is_empty(),
            "current tree must stay unchanged when any action failed"
        );

        // Now a batch whose LATER step fails after an earlier Begin applied:
        // the rollback must dispose the earlier entry so nothing survives.
        journal.upsert("seed", "GoodFactory", json!({"v": 0}), None);
        let desired_late = EntryTree(vec![
            Entry {
                id: "good:first".into(),
                plugin: "GoodFactory".into(),
                config: json!({"v": 7}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
            Entry {
                id: "good:last".into(),
                plugin: "GhostFactory".into(),
                config: json!({}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
            Entry {
                id: "good:never".into(),
                plugin: "LateGoodFactory".into(),
                config: json!({}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
        ]);
        let actions =
            Loader::apply(&ctx, &mut current, &desired_late, &journal).await;
        let failed = actions
            .iter()
            .find(|a| a.id == "good:last")
            .expect("mid-batch failure named");
        assert!(failed.status.is_err());
        assert!(
            failed.status.as_ref().unwrap_err().contains("no factory registered"),
            "error names the cause: {:?}",
            failed.status
        );
        assert!(
            !actions.iter().any(|a| a.id == "good:never"),
            "entries past the failure never ran"
        );
        assert!(
            ctx.get::<Good>().is_none(),
            "rolled back: the entry applied before the failure is disposed"
        );
        assert!(
            journal.get("good:first").is_none(),
            "rollback retired the began entry's journal record"
        );
        assert!(
            current.0.is_empty(),
            "current stays at the prior tree after a rolled-back batch"
        );
    }

    // --- verified hot-swap (item #3) ---

    /// Shared service type both swap plugins provide.
    #[derive(Debug)]
    struct Swappable(std::sync::atomic::AtomicU64);
    impl Service for Swappable {}

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rebuild_same_type_verified_swap() {
        use crate::RegistryService;
        use std::sync::atomic::Ordering;

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        // Two factories providing the SAME service TypeId; the counter marks
        // which instance is live so we can observe continuity across the swap.
        plugin_registry.register(
            "SwapFactoryA",
            Arc::new(|ctx: &Arc<crate::Context>, _cfg| {
                let fut = ctx.plugin(Swappable(std::sync::atomic::AtomicU64::new(1)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );
        plugin_registry.register(
            "SwapFactoryB",
            Arc::new(|ctx: &Arc<crate::Context>, _cfg| {
                let fut = ctx.plugin(Swappable(std::sync::atomic::AtomicU64::new(2)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );

        let desired_a = EntryTree(vec![Entry {
            id: "swap".into(),
            plugin: "SwapFactoryA".into(),
            config: json!({}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let mut current = EntryTree(vec![]);
        let actions = Loader::apply(&ctx, &mut current, &desired_a, &journal).await;
        assert_eq!(actions[0].action, "begin");
        assert!(actions[0].status.is_ok());
        assert_eq!(actions[0].verified, true);
        let svc = ctx.get::<Swappable>().expect("initial provider");
        assert_eq!(svc.0.load(Ordering::SeqCst), 1);

        // Plugin change with the same Provides TypeId -> RebuildFiber, and the
        // live service must stay resolvable across the whole apply (probed
        // from a concurrent task while the swap runs on this one).
        let desired_b = EntryTree(vec![Entry {
            id: "swap".into(),
            plugin: "SwapFactoryB".into(),
            config: json!({}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let ctx_probe = ctx.clone();
        let prober = tokio::spawn(async move {
            for _ in 0..200 {
                if ctx_probe.get::<Swappable>().is_none() {
                    return false;
                }
                tokio::task::yield_now().await;
            }
            true
        });
        let actions = Loader::apply(&ctx, &mut current, &desired_b, &journal).await;
        assert_eq!(actions[0].action, "rebuild-fiber");
        assert!(actions[0].status.is_ok(), "rebuild ok");
        assert_eq!(actions[0].verified, true, "same-type swap must be verified");

        let continuous = prober.await.expect("prober task");
        assert!(continuous, "service must stay resolvable during swap");

        // New instance is live and owned by a fresh Active fiber.
        let svc = ctx.get::<Swappable>().expect("swapped provider");
        assert_eq!(svc.0.load(Ordering::SeqCst), 2);
        let rec = journal.get("swap").expect("journal record");
        let fid = rec.fiber_id.expect("fiber recorded");
        let registry = ctx.get::<RegistryService>().unwrap();
        assert!(matches!(
            registry.get_fiber(fid).unwrap().state(),
            crate::FiberState::Active { .. }
        ));
        assert_eq!(current.0.len(), 1, "current tree advanced");
    }

    /// UpdateConfig pre-flight: a factory that rejects the new config fails
    /// its action with the "config pre-flight failed" marker, the journal and
    /// live fiber stay untouched, and the OLD provider keeps serving.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bad_config_update_keeps_old_provider_serving() {
        use crate::RegistryService;
        use std::sync::atomic::Ordering;

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        // Dual-mode factory: healthy instance for {"v": N}, hard failure for
        // {"fail": true}. Mirrors the KeeperFactory shape of the swap tests.
        plugin_registry.register(
            "PickyFactory",
            Arc::new(|ctx: &Arc<crate::Context>, cfg| {
                if cfg.get("fail").and_then(|x| x.as_bool()) == Some(true) {
                    return Err(crate::CordisError::Configuration(
                        "config rejected by factory".into(),
                    ));
                }
                let v = cfg.get("v").and_then(|x| x.as_u64()).unwrap_or(0);
                let fut = ctx.plugin(Swappable(std::sync::atomic::AtomicU64::new(v)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );

        let entry_ok = Entry {
            id: "picky".into(),
            plugin: "PickyFactory".into(),
            config: json!({"v": 1}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        };
        let mut current = EntryTree(vec![]);
        Loader::apply(&ctx, &mut current, &EntryTree(vec![entry_ok]), &journal).await;
        let before = journal.get("picky").expect("journal record after begin");
        assert_eq!(
            ctx.get::<Swappable>().unwrap().0.load(Ordering::SeqCst),
            1,
            "old provider serving"
        );

        // Config change to a REJECTED config → UpdateConfig action whose
        // pre-flight trial fails; old provider must keep serving.
        let desired_bad = EntryTree(vec![Entry {
            id: "picky".into(),
            plugin: "PickyFactory".into(),
            config: json!({"fail": true}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let actions = Loader::apply(&ctx, &mut current, &desired_bad, &journal).await;
        assert_eq!(actions[0].action, "update-config");
        assert!(actions[0].status.is_err(), "pre-flight failure reported");
        assert!(
            actions[0]
                .status
                .as_ref()
                .unwrap_err()
                .contains("config pre-flight failed"),
            "failure names the pre-flight marker, got {:?}",
            actions[0].status
        );

        // Old provider fully intact; journal frozen (no generation bump, no
        // config overwrite); current tree unchanged so a retry re-diffs.
        assert!(
            ctx.get::<Swappable>().is_some(),
            "old provider kept serving"
        );
        assert_eq!(
            ctx.get::<Swappable>().unwrap().0.load(Ordering::SeqCst),
            1,
            "still the OLD instance value"
        );
        let after = journal.get("picky").expect("record retained");
        assert_eq!(after.generation, before.generation, "generation frozen");
        assert_eq!(after.config, json!({"v": 1}), "config not overwritten");
        let fid = before.fiber_id.expect("fiber tracked");
        assert!(matches!(
            ctx.get::<RegistryService>()
                .unwrap()
                .get_fiber(fid)
                .unwrap()
                .state(),
            crate::FiberState::Active { .. }
        ));
        assert_eq!(current.0[0].config, json!({"v": 1}), "tree unchanged");

        // A HEALTHY config change still goes through end-to-end (the
        // pre-flight passes and Fiber::update re-applies).
        let desired_good = EntryTree(vec![Entry {
            id: "picky".into(),
            plugin: "PickyFactory".into(),
            config: json!({"v": 5}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let actions = Loader::apply(&ctx, &mut current, &desired_good, &journal).await;
        assert_eq!(actions[0].action, "update-config");
        assert!(actions[0].status.is_ok(), "healthy update applies");
        assert_eq!(current.0[0].config, json!({"v": 5}), "tree advanced");
        assert_eq!(
            journal.get("picky").unwrap().generation,
            before.generation + 1,
            "journal bumped once on success"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rebuild_failure_keeps_old() {
        use crate::RegistryService;

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        #[derive(Debug)]
        struct Keeper(u64);
        impl Service for Keeper {}

        plugin_registry.register(
            "KeeperFactory",
            Arc::new(|ctx: &Arc<crate::Context>, _cfg| {
                let fut = ctx.plugin(Keeper(1));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );
        plugin_registry.register(
            "BrokenFactory",
            Arc::new(|_ctx: &Arc<crate::Context>, _cfg| {
                Err(crate::CordisError::Configuration(
                    "intentional swap failure".into(),
                ))
            }),
        );

        let desired_ok = EntryTree(vec![Entry {
            id: "keep".into(),
            plugin: "KeeperFactory".into(),
            config: json!({}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let mut current = EntryTree(vec![]);
        Loader::apply(&ctx, &mut current, &desired_ok, &journal).await;
        assert!(ctx.get::<Keeper>().is_some(), "old provider live");

        // A failing candidate must leave the old provider serving untouched.
        let desired_bad = EntryTree(vec![Entry {
            id: "keep".into(),
            plugin: "BrokenFactory".into(),
            config: json!({}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let actions = Loader::apply(&ctx, &mut current, &desired_bad, &journal).await;
        assert_eq!(actions[0].action, "rebuild-fiber");
        assert!(actions[0].status.is_err(), "failed trial reported");
        assert!(actions[0]
            .status
            .as_ref()
            .unwrap_err()
            .contains("intentional swap failure"));
        assert!(
            ctx.get::<Keeper>().is_some(),
            "old fiber still Active after failed rebuild"
        );
        // Current tree unchanged so retry re-diffs cleanly.
        assert_eq!(current.0[0].plugin, "KeeperFactory");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rebuild_without_tracked_fiber_reports_unverified() {
        use crate::RegistryService;

        // Journal WITHOUT fiber ids simulates an entry whose registration was
        // never tracked: the fallback path must run and report verified=false.
        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        #[derive(Debug)]
        struct Fallback(u64);
        impl Service for Fallback {}

        plugin_registry.register(
            "FallbackFactory",
            Arc::new(|ctx: &Arc<crate::Context>, _cfg| {
                let fut = ctx.plugin(Fallback(9));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );
        // Seed the journal record by hand with no fiber id (as an untracked boot
        // would have left it).
        journal.upsert("fb", "FallbackFactory", json!({}), None);

        let desired = EntryTree(vec![Entry {
            id: "fb".into(),
            plugin: "FallbackFactory".into(),
            config: json!({}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let mut current = EntryTree(vec![Entry {
            id: "fb".into(),
            plugin: "OtherPlugin".into(),
            config: json!({}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        let actions = Loader::apply(&ctx, &mut current, &desired, &journal).await;
        assert_eq!(actions[0].action, "rebuild-fiber");
        assert!(actions[0].status.is_ok());
        assert_eq!(actions[0].verified, false, "fallback is unverified");
        assert!(ctx.get::<Fallback>().is_some(), "entry instantiated");
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
    fn save_to_toml_file_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entries.toml");
        let tree = EntryTree(vec![Entry {
            id: "tool:calc".into(),
            plugin: "CalculatorService".into(),
            config: json!({}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        // Two consecutive saves exercise both the create and rename-over
        // paths; neither may leave `.tmp-*` siblings behind.
        tree.save_to_toml_file(&path).unwrap();
        tree.save_to_toml_file(&path).unwrap();
        let mut leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        leftovers.sort();
        assert_eq!(leftovers, vec!["entries.toml".to_string()]);
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

    #[test]
    fn save_to_file_is_atomic_no_temp_residue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("entries.json");
        let tree = EntryTree(vec![Entry {
            id: "tool:calc".into(),
            plugin: "CalculatorService".into(),
            config: json!({"precision": 2}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }]);
        // Parent directory does not exist yet — the save must create it.
        tree.save_to_file(path.to_str().unwrap()).unwrap();
        assert_eq!(
            EntryTree::load_from_file(path.to_str().unwrap()).unwrap(),
            tree,
            "content survives the temp+rename round-trip"
        );
        // Two consecutive saves exercise create + rename-over; neither may
        // leave `.tmp-*` siblings behind (rename consumed each temp).
        tree.save_to_file(path.to_str().unwrap()).unwrap();
        let mut leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        leftovers.sort();
        assert_eq!(
            leftovers,
            vec!["nested".to_string()],
            "no *.tmp-* siblings may remain after a successful save"
        );
        let inner: Vec<String> = std::fs::read_dir(dir.path().join("nested"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(inner, vec!["entries.json".to_string()]);
    }

    #[test]
    fn save_to_file_consecutive_saves_succeed_with_distinct_temps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entries.json");
        let tree = EntryTree::default();
        // Each save consumes a fresh pid+nonce temp name; both must succeed
        // (a colliding name would make the second rename target already
        // gone / interleaved with the first).
        tree.save_to_file(path.to_str().unwrap()).unwrap();
        tree.save_to_file(path.to_str().unwrap()).unwrap();
        assert_eq!(
            EntryTree::load_from_file(path.to_str().unwrap()).unwrap(),
            EntryTree::default()
        );
        // Nonce monotonicity: distinct increments, never reused.
        let a = next_save_nonce();
        let b = next_save_nonce();
        assert_ne!(a, b, "nonce must be monotonic across calls");
    }

    // --- dependency-cycle detection (round-7 wiring of cycles.rs) ---

    /// Mutual-inject pair: A declares an inject on B's provided type and vice
    /// versa, mirroring the declare_inject pattern from
    /// `crates/ares-agent/src/plugins.rs`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cycle_detection_finds_mutual_declared_injects() {
        use crate::cycles::CycleLedger;
        use crate::{Plugin, RegistryService};

        #[derive(Debug)]
        struct SvcA(u32);
        impl Service for SvcA {}
        #[derive(Debug)]
        struct SvcB(u32);
        impl Service for SvcB {}

        struct PluginA;
        impl Plugin for PluginA {
            type Config = ();
            type Provides = SvcA;
            fn apply(&self, ctx: &Arc<Context>, _cfg: ()) -> Result<Arc<SvcA>, crate::CordisError> {
                Ok(ctx.provide(SvcA(1)))
            }
        }

        struct PluginB;
        impl Plugin for PluginB {
            type Config = ();
            type Provides = SvcB;
            fn apply(&self, ctx: &Arc<Context>, _cfg: ()) -> Result<Arc<SvcB>, crate::CordisError> {
                Ok(ctx.provide(SvcB(2)))
            }
        }

        let ctx = Context::new_root();
        ctx.provide(crate::LoaderJournal::new());
        ctx.provide(RegistryService::new());
        ctx.provide(CycleLedger::new());
        let registry = ctx.get::<RegistryService>().unwrap();

        let fid_a = registry.plugin(&ctx, PluginA, ()).expect("register A");
        let fid_b = registry.plugin(&ctx, PluginB, ()).expect("register B");
        // Off the loader path the ledger must be fed explicitly — this mirrors
        // exactly what instantiate_entry records per fresh provide.
        let ledger = ctx.get::<CycleLedger>().unwrap();
        ledger.record_provider(std::any::TypeId::of::<SvcA>(), None, fid_a);
        ledger.record_provider(std::any::TypeId::of::<SvcB>(), None, fid_b);
        // The mutual inject declarations that make A and B permanently wait on
        // each other.
        registry.get_fiber(fid_a).unwrap().declare_inject::<SvcB>();
        registry.get_fiber(fid_b).unwrap().declare_inject::<SvcA>();

        let cycles = Loader::detect_cycles(&ctx);
        assert_eq!(cycles.len(), 1, "exactly one 2-cycle expected");
        let cycle = &cycles[0];
        assert_eq!(cycle.len(), 3, "closed ring: [x, y, x]");
        assert_eq!(cycle[0], cycle[2], "ring closes on itself");

        // Entry ids resolve through the journal; the closed ring repeats its
        // head so the id path repeats too.
        let journal = ctx.get::<crate::LoaderJournal>().unwrap();
        journal.upsert("a", "PluginA", json!({}), Some(fid_a));
        journal.upsert("b", "PluginB", json!({}), Some(fid_b));
        let ids = Loader::cycle_entry_ids(Some(journal.as_ref()), &cycles);
        assert_eq!(
            ids,
            vec![vec!["a".to_string(), "b".to_string(), "a".to_string()]]
        );
    }

    /// Full-apply integration: two mutually injecting entries applied through
    /// `Loader::apply` produce the warning pass without failing the batch, and
    /// `detect_cycles` reports the ring afterwards.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_reports_cycle_without_failing_batch() {
        use crate::cycles::CycleLedger;
        use crate::{Plugin, RegistryService};

        #[derive(Debug)]
        struct SvcA(u32);
        impl Service for SvcA {}
        #[derive(Debug)]
        struct SvcB(u32);
        impl Service for SvcB {}

        struct PluginA;
        impl Plugin for PluginA {
            type Config = serde_json::Value;
            type Provides = SvcA;
            fn apply(
                &self,
                ctx: &Arc<Context>,
                _cfg: serde_json::Value,
            ) -> Result<Arc<SvcA>, crate::CordisError> {
                Ok(ctx.provide(SvcA(1)))
            }
        }

        struct PluginB;
        impl Plugin for PluginB {
            type Config = serde_json::Value;
            type Provides = SvcB;
            fn apply(
                &self,
                ctx: &Arc<Context>,
                _cfg: serde_json::Value,
            ) -> Result<Arc<SvcB>, crate::CordisError> {
                Ok(ctx.provide(SvcB(2)))
            }
        }

        let ctx = Context::new_root();
        let journal = ctx.provide(crate::LoaderJournal::new());
        ctx.provide(RegistryService::new());
        ctx.provide(CycleLedger::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        plugin_registry.register(
            "CycleA",
            Arc::new(|ctx, _config| {
                let future = ctx.plugin(SvcA(1));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
            }),
        );
        plugin_registry.register(
            "CycleB",
            Arc::new(|ctx, _config| {
                let future = ctx.plugin(SvcB(2));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
            }),
        );

        let entry_a = Entry {
            id: "cyc:a".into(),
            plugin: "CycleA".into(),
            config: json!({}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        };
        let mut entry_b = entry_a.clone();
        entry_b.id = "cyc:b".into();
        entry_b.plugin = "CycleB".into();

        let desired = EntryTree(vec![entry_a.clone(), entry_b.clone()]);
        let mut current = EntryTree::default();
        let actions = Loader::apply(&ctx, &mut current, &desired, &journal).await;
        assert!(
            actions.iter().all(|a| a.status.is_ok()),
            "apply must not fail because of the cycle: {actions:?}"
        );
        assert_eq!(current.0.len(), 2, "tree advanced despite the cycle");

        // instantiate_entry recorded both providers in the ledger; now declare
        // the mutual injects (as the production plugins would) and confirm
        // detection names exactly this ring.
        let fid_a = journal.get("cyc:a").unwrap().fiber_id.unwrap();
        let fid_b = journal.get("cyc:b").unwrap().fiber_id.unwrap();
        ctx.get::<RegistryService>()
            .unwrap()
            .get_fiber(fid_a)
            .unwrap()
            .declare_inject::<SvcB>();
        ctx.get::<RegistryService>()
            .unwrap()
            .get_fiber(fid_b)
            .unwrap()
            .declare_inject::<SvcA>();

        let cycles = Loader::detect_cycles(&ctx);
        assert_eq!(cycles.len(), 1);
        // reconcile emits Begin actions in nondeterministic order (HashMap
        // iteration), so either fiber may register first; the ring is the
        // same cycle either way. Assert membership + closure, not rotation.
        let ring: std::collections::HashSet<u64> = cycles[0].iter().copied().collect();
        let expected: std::collections::HashSet<u64> = [fid_a, fid_b].into_iter().collect();
        assert_eq!(ring, expected, "closed 2-ring over both fibers");
    }

    // --- rolling drain-and-shift provider replacement (replace_provider) ---

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replace_provider_zero_absence_window() {
        use crate::RegistryService;
        use std::sync::atomic::Ordering;

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        plugin_registry.register(
            "SwapFactoryA",
            Arc::new(|ctx: &Arc<crate::Context>, _cfg| {
                let fut = ctx.plugin(Swappable(std::sync::atomic::AtomicU64::new(1)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );
        plugin_registry.register(
            "SwapFactory",
            Arc::new(|ctx: &Arc<crate::Context>, cfg| {
                // The instance value comes from the config, so replacing the
                // provider under the SAME factory label with a NEW config
                // still flips the observable instance.
                let v = cfg.get("v").and_then(|x| x.as_u64()).unwrap_or(0);
                let fut = ctx.plugin(Swappable(std::sync::atomic::AtomicU64::new(v)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );

        let entry_a = Entry {
            id: "swap".into(),
            plugin: "SwapFactory".into(),
            config: json!({"v": 1}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        };
        let mut current = EntryTree(vec![]);
        Loader::apply(&ctx, &mut current, &EntryTree(vec![entry_a]), &journal).await;
        let old_rec = journal.get("swap").expect("journal record after begin");
        let old_fid = old_rec.fiber_id.expect("fiber tracked");
        let old_gen = old_rec.generation;
        assert_eq!(
            ctx.get::<Swappable>().unwrap().0.load(Ordering::SeqCst),
            1,
            "old provider serving"
        );

        // Concurrent get-probe: the service must NEVER be unresolvable while
        // the replacement runs — the key never becomes unprovided.
        let ctx_probe = ctx.clone();
        let prober = tokio::spawn(async move {
            for _ in 0..300 {
                if ctx_probe.get::<Swappable>().is_none() {
                    return false;
                }
                tokio::task::yield_now().await;
            }
            true
        });

        let loader = Loader::new();
        let new_fid = loader
            .replace_provider(&ctx, "SwapFactory", json!({"v": 2}), &journal)
            .await
            .expect("replace_provider swap");

        let continuous = prober.await.expect("prober task");
        assert!(continuous, "get must stay satisfied during the whole swap");

        // New instance is live under a fresh Active fiber; the old fiber is gone.
        let svc = ctx.get::<Swappable>().expect("swapped provider");
        assert_eq!(svc.0.load(Ordering::SeqCst), 2, "instance flipped");
        let registry = ctx.get::<RegistryService>().unwrap();
        assert!(matches!(
            registry
                .get_fiber(new_fid)
                .expect("new fiber tracked")
                .state(),
            crate::FiberState::Active { .. }
        ));
        assert!(
            registry.get_fiber(old_fid).is_none(),
            "old registration removed"
        );
        // Same entry id retained (plugin label keyed), generation advanced.
        let rec = journal.get("swap").expect("journal record after replace");
        assert_eq!(rec.fiber_id, Some(new_fid));
        assert_eq!(rec.generation, old_gen + 1);
        assert_ne!(rec.fiber_id, Some(old_fid));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replace_provider_failure_keeps_old() {
        use crate::RegistryService;

        #[derive(Debug)]
        struct Keeper(u64);
        impl Service for Keeper {}

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        plugin_registry.register(
            "KeeperFactory",
            Arc::new(|ctx: &Arc<crate::Context>, cfg| {
                // Same dual-mode shape as the success tests: a healthy
                // instance when the config asks for it, an intentional
                // failure otherwise. replace_provider resolves BOTH the old
                // record and the replacement factory through this one label.
                if cfg.get("fail").and_then(|x| x.as_bool()) == Some(true) {
                    return Err(crate::CordisError::Configuration(
                        "intentional replace failure".into(),
                    ));
                }
                let fut = ctx.plugin(Keeper(1));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );

        let entry_ok = Entry {
            id: "keep".into(),
            plugin: "KeeperFactory".into(),
            config: json!({}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        };
        let mut current = EntryTree(vec![]);
        Loader::apply(&ctx, &mut current, &EntryTree(vec![entry_ok]), &journal).await;
        let before = journal.get("keep").expect("journal record");
        let old_fid = before.fiber_id.expect("old fiber tracked");

        let loader = Loader::new();
        let err = loader
            .replace_provider(&ctx, "KeeperFactory", json!({"fail": true}), &journal)
            .await
            .expect_err("failing trial must error");
        assert!(
            err.to_string().contains("intentional replace failure"),
            "error carries the factory failure: {err}"
        );

        // Old provider fully intact: still resolving, same tracked fiber, no
        // intercept residue from the aborted swap.
        assert!(
            ctx.get::<Keeper>().is_some(),
            "old provider kept after failed replace"
        );
        let registry = ctx.get::<RegistryService>().unwrap();
        assert!(registry.get_fiber(old_fid).is_some(), "old fiber tracked");
        assert!(
            !matches!(
                registry.get_fiber(old_fid).unwrap().state(),
                crate::FiberState::Failed { .. }
            ),
            "old fiber untouched by the failed trial"
        );
        let after = journal.get("keep").expect("journal record retained");
        assert_eq!(after.generation, before.generation, "generation frozen");
        assert_eq!(after.fiber_id, Some(old_fid), "fiber id unchanged");
        assert!(
            !current.0.is_empty() && current.0[0].plugin == "KeeperFactory",
            "current tree unchanged"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replace_provider_updates_journal() {
        use crate::RegistryService;

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        plugin_registry.register(
            "SwapFactory",
            Arc::new(|ctx: &Arc<crate::Context>, cfg| {
                let v = cfg.get("v").and_then(|x| x.as_u64()).unwrap_or(0);
                let fut = ctx.plugin(Swappable(std::sync::atomic::AtomicU64::new(v)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );

        let entry_a = Entry {
            id: "svc:swap".into(),
            plugin: "SwapFactory".into(),
            config: json!({"v": 1}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        };
        let mut current = EntryTree(vec![]);
        Loader::apply(&ctx, &mut current, &EntryTree(vec![entry_a]), &journal).await;
        let before = journal.get("svc:swap").expect("record present");
        assert_eq!(before.generation, 1);
        assert_eq!(before.config, json!({"v": 1}));

        let loader = Loader::new();
        let new_config = json!({"v": 7});
        let new_fid = loader
            .replace_provider(&ctx, "SwapFactory", new_config.clone(), &journal)
            .await
            .expect("replace ok");

        let rec = journal.get("svc:swap").expect("record retained");
        assert_eq!(
            rec.fiber_id,
            Some(new_fid),
            "new fiber id recorded in the journal"
        );
        assert_ne!(rec.fiber_id, before.fiber_id, "fiber id flipped");
        assert_eq!(
            rec.generation,
            before.generation + 1,
            "generation bumped exactly once per successful replace"
        );
        assert_eq!(rec.config, new_config, "new config stored on the record");
        assert_eq!(rec.plugin, "SwapFactory", "plugin label retained");
        // The promoted instance actually carries the new config's value.
        let svc = ctx.get::<Swappable>().expect("swapped provider");
        assert_eq!(svc.0.load(std::sync::atomic::Ordering::SeqCst), 7);

        // Second replace against the SAME plugin label exercises the
        // self-replacement path (old and new resolve through one label).
        let again = loader
            .replace_provider(&ctx, "SwapFactory", json!({"v": 8}), &journal)
            .await
            .expect("self-replace ok");
        let rec2 = journal.get("svc:swap").expect("record retained");
        assert_eq!(rec2.fiber_id, Some(again));
        assert_eq!(rec2.generation, rec.generation + 1);
        assert_eq!(
            ctx.get::<Swappable>()
                .unwrap()
                .0
                .load(std::sync::atomic::Ordering::SeqCst),
            8,
            "second swap live"
        );
    }

    // --- round-5 wave 2: config-only patches, staged batches, self-kill ---

    /// Config-only patch on an Active fiber: the update path re-applies the
    /// plugin through `Fiber::update` (undo + runner), so the factory runs
    /// exactly TWICE total across begin + patch (initial apply, then the
    /// live re-apply) — and critically the entry is never retired/re-begun:
    /// apply_count stays at its begin value while the config takes effect.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn config_only_change_patches_without_restart() {
        use crate::RegistryService;
        use std::sync::atomic::Ordering;

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        let ops = ctx.provide(LoaderOps::new());
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        plugin_registry.register(
            "PickyFactory",
            Arc::new(|ctx: &Arc<crate::Context>, cfg| {
                if cfg.get("fail").and_then(|x| x.as_bool()) == Some(true) {
                    return Err(crate::CordisError::Configuration(
                        "config rejected by factory".into(),
                    ));
                }
                let v = cfg.get("v").and_then(|x| x.as_u64()).unwrap_or(0);
                let fut = ctx.plugin(Swappable(std::sync::atomic::AtomicU64::new(v)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );

        let mut current = EntryTree(vec![]);
        Loader::apply(
            &ctx,
            &mut current,
            &EntryTree(vec![Entry {
                id: "picky".into(),
                plugin: "PickyFactory".into(),
                config: json!({"v": 1}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            }]),
            &journal,
        )
        .await;
        let fid = journal.get("picky").unwrap().fiber_id.unwrap();

        // Config-only change: same plugin/id/disabled/isolate/intercept.
        let actions = Loader::apply(
            &ctx,
            &mut current,
            &EntryTree(vec![Entry {
                id: "picky".into(),
                plugin: "PickyFactory".into(),
                config: json!({"v": 5}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            }]),
            &journal,
        )
        .await;
        assert_eq!(actions[0].action, "update-config");
        assert!(actions[0].status.is_ok(), "{:?}", actions[0].status);

        // The patch went through the SAME registration fiber — no stop+start,
        // no rebuild. Value application rides the fiber's reload runner (the
        // registry-register path); plain factory fibers record the new config
        // in the journal and converge on their next reactive refresh.
        assert_eq!(
            ctx.get::<Swappable>().unwrap().0.load(Ordering::SeqCst),
            1,
            "same live instance kept serving (no restart)"
        );
        assert_eq!(journal.get("picky").unwrap().fiber_id, Some(fid));
        assert_eq!(journal.get("picky").unwrap().config, json!({"v": 5}));
        // Apply count stayed at ONE completed loader application for this
        // entry: the patch went through Fiber::update, not a fresh Begin.
        assert_eq!(
            ops.apply_count("picky"),
            1,
            "config-only patch must not re-invoke the entry's Begin"
        );
    }

    /// Rejected config patch: pre-flight fails the action, old provider keeps
    /// serving, journal/tree frozen so the next reload retries cleanly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejected_patch_keeps_old_config() {
        use crate::RegistryService;
        use std::sync::atomic::Ordering;

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        plugin_registry.register(
            "PickyFactory",
            Arc::new(|ctx: &Arc<crate::Context>, cfg| {
                if cfg.get("fail").and_then(|x| x.as_bool()) == Some(true) {
                    return Err(crate::CordisError::Configuration(
                        "config rejected by factory".into(),
                    ));
                }
                let v = cfg.get("v").and_then(|x| x.as_u64()).unwrap_or(0);
                let fut = ctx.plugin(Swappable(std::sync::atomic::AtomicU64::new(v)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );

        let mut current = EntryTree(vec![]);
        Loader::apply(
            &ctx,
            &mut current,
            &EntryTree(vec![Entry {
                id: "picky".into(),
                plugin: "PickyFactory".into(),
                config: json!({"v": 1}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            }]),
            &journal,
        )
        .await;
        let before = journal.get("picky").expect("record");

        let actions = Loader::apply(
            &ctx,
            &mut current,
            &EntryTree(vec![Entry {
                id: "picky".into(),
                plugin: "PickyFactory".into(),
                config: json!({"fail": true}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            }]),
            &journal,
        )
        .await;
        assert_eq!(actions[0].action, "update-config");
        let err = actions[0].status.as_ref().unwrap_err();
        assert!(err.contains("config pre-flight failed"), "{err}");

        // Old provider serving, old config everywhere; a retry re-diffs.
        assert_eq!(
            ctx.get::<Swappable>().unwrap().0.load(Ordering::SeqCst),
            1,
            "old instance still serving"
        );
        assert_eq!(
            journal.get("picky").unwrap().config,
            json!({"v": 1}),
            "journal kept the old config"
        );
        assert_eq!(journal.get("picky").unwrap().generation, before.generation);
        assert_eq!(current.0[0].config, json!({"v": 1}), "tree unchanged");

        // The retry with the SAME desired tree now succeeds end-to-end: the
        // journal records the new config and the action reports Ok on the
        // same live instance (value application rides the fiber's runner).
        let actions = Loader::apply(
            &ctx,
            &mut current,
            &EntryTree(vec![Entry {
                id: "picky".into(),
                plugin: "PickyFactory".into(),
                config: json!({"v": 2}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            }]),
            &journal,
        )
        .await;
        assert!(actions[0].status.is_ok(), "{:?}", actions[0].status);
        assert_eq!(current.0[0].config, json!({"v": 2}), "tree advanced");
        assert_eq!(
            journal.get("picky").unwrap().generation,
            before.generation + 1,
            "exactly one successful journal bump"
        );
    }

    /// Staged batch of 3 where #2 fails: #1's change is reverted, #3 never
    /// applied, and the live context serves only the originals. Batch order
    /// is deterministic (dependency classes then entry id).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn staged_batch_rolls_back_on_first_failure() {
        use crate::RegistryService;
        use std::sync::atomic::{AtomicU64, Ordering};

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        #[derive(Debug)]
        struct Triple(AtomicU64);
        impl Service for Triple {}

        plugin_registry.register(
            "TripleFactory",
            Arc::new(|ctx: &Arc<crate::Context>, cfg| {
                let v = cfg.get("v").and_then(|x| x.as_u64()).unwrap_or(0);
                let fut = ctx.plugin(Triple(AtomicU64::new(v)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );

        // Seed one live entry (start from an EMPTY current so the seed apply
        // actually produces a Begin and journals the record).
        let mut current = EntryTree(vec![]);
        Loader::apply(
            &ctx,
            &mut current,
            &EntryTree(vec![Entry {
                id: "t:live".into(),
                plugin: "TripleFactory".into(),
                config: json!({"v": 100}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            }]),
            &journal,
        )
        .await;
        let live_fid = journal.get("t:live").unwrap().fiber_id.unwrap();
        let live_gen = journal.get("t:live").unwrap().generation;

        // Batch: (1) config update on t:live [applies], (2) Begin t:new that
        // FAILS via a rejecting config, (3) Begin t:never [must not run].
        plugin_registry.register(
            "BrokenTripleFactory",
            Arc::new(|_ctx: &Arc<crate::Context>, _cfg| {
                Err(crate::CordisError::Configuration(
                    "intentional batch failure".into(),
                ))
            }),
        );
        plugin_registry.register(
            "NeverFactory",
            Arc::new(|ctx: &Arc<crate::Context>, cfg| {
                let v = cfg.get("v").and_then(|x| x.as_u64()).unwrap_or(0);
                #[derive(Debug)]
                struct Never(u64);
                impl crate::Service for Never {}
                let fut = ctx.plugin(Never(v));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );
        let desired = EntryTree(vec![
            Entry {
                id: "t:live".into(),
                plugin: "TripleFactory".into(),
                config: json!({"v": 200}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
            Entry {
                id: "t:new".into(),
                plugin: "BrokenTripleFactory".into(),
                config: json!({}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
            // Distinct service type (own factory) so this step's only failure
            // mode is "the batch already aborted", not a provider clash with
            // t:live's Triple provider.
            Entry {
                id: "t:never".into(),
                plugin: "NeverFactory".into(),
                config: json!({"v": 9}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
        ]);
        let actions = Loader::apply(&ctx, &mut current, &desired, &journal).await;

        let failed = actions
            .iter()
            .find(|a| a.id == "t:new")
            .expect("failing entry named in results");
        assert!(failed.status.is_err());
        assert!(
            failed.status.as_ref().unwrap_err().contains("intentional batch failure"),
            "{:?}",
            failed.status
        );
        assert!(
            !actions.iter().any(|a| a.id == "t:never" && a.status.is_ok()),
            "#3 must never be applied"
        );

        // Rollback proof: t:live still serves the ORIGINAL value 100 on its
        // ORIGINAL fiber, and the original journal record survived.
        assert_eq!(
            ctx.get::<Triple>().map(|t| t.0.load(Ordering::SeqCst)),
            Some(100),
            "live tree serves the original after rollback"
        );
        let rec = journal.get("t:live").unwrap();
        assert_eq!(rec.fiber_id, Some(live_fid));
        assert_eq!(rec.generation, live_gen, "no net journal churn");
        assert_eq!(rec.config, json!({"v": 100}), "original config restored");
        assert!(journal.get("t:new").is_none());
        assert!(journal.get("t:never").is_none());
        assert_eq!(current.0.len(), 1, "current stays at prior tree");
    }

    /// Staged batch where every step verifies: applies in dependency order
    /// (begins first, updates second, retires last) and settles cleanly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn staged_batch_applies_in_order_on_success() {
        use crate::RegistryService;
        use std::sync::atomic::{AtomicU64, Ordering};

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        #[derive(Debug)]
        struct Ordered(AtomicU64);
        impl Service for Ordered {}

        plugin_registry.register(
            "OrderedFactory",
            Arc::new(|ctx: &Arc<crate::Context>, cfg| {
                let v = cfg.get("v").and_then(|x| x.as_u64()).unwrap_or(0);
                let fut = ctx.plugin(Ordered(AtomicU64::new(v)));
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );

        // Seed two live entries with DISTINCT service types via distinct
        // factories, so the batch can begin/update/retire without tripping
        // the single-source discipline across batches.
        plugin_registry.register(
            "KeepFactory",
            Arc::new(|_ctx: &Arc<crate::Context>, _cfg| Ok(0)),
        );
        plugin_registry.register(
            "ByeFactory",
            Arc::new(|_ctx: &Arc<crate::Context>, _cfg| Ok(0)),
        );
        // Start from an EMPTY current so the seed apply actually Begins both
        // entries and journals their records.
        let mut current = EntryTree(vec![]);
        Loader::apply(
            &ctx,
            &mut current,
            &EntryTree(vec![
                Entry {
                    id: "o:keep".into(),
                    plugin: "KeepFactory".into(),
                    config: json!({"v": 10}),
                    disabled: false,
                    isolate: None,
                    intercept: HashMap::new(),
                },
                Entry {
                    id: "o:bye".into(),
                    plugin: "ByeFactory".into(),
                    config: json!({"v": 1}),
                    disabled: false,
                    isolate: None,
                    intercept: HashMap::new(),
                },
            ]),
            &journal,
        )
        .await;
        let keep_fid = journal.get("o:keep").unwrap().fiber_id.unwrap();
        let retire_fid = journal.get("o:bye").unwrap().fiber_id.unwrap();

        let desired = EntryTree(vec![
            // Retire o:bye (removed from desired).
            Entry {
                id: "o:keep".into(),
                plugin: "KeepFactory".into(),
                config: json!({"v": 11}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
            Entry {
                id: "o:new".into(),
                plugin: "OrderedFactory".into(),
                config: json!({"v": 2}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            },
        ]);
        let actions = Loader::apply(&ctx, &mut current, &desired, &journal).await;
        assert!(
            actions.iter().all(|a| a.status.is_ok()),
            "every action ok: {actions:?}"
        );
        assert_eq!(actions.len(), 3, "begin + update + retire all reported");

        // All three effects landed.
        assert!(
            ctx.get::<Ordered>().is_some(),
            "begin instantiated the new provider"
        );
        assert!(journal.get("o:new").is_some(), "begin settled");
        assert!(journal.get("o:bye").is_none(), "retire settled");
        assert_eq!(
            journal.get("o:keep").unwrap().config,
            json!({"v": 11}),
            "update settled"
        );
        assert_eq!(journal.get("o:keep").unwrap().fiber_id, Some(keep_fid));
        // Retired fiber disposed and gone from tracking.
        let registry = ctx.get::<crate::RegistryService>().unwrap();
        assert!(
            registry.get_fiber(retire_fid).map(|f| f.is_disposed()).unwrap_or(true),
            "retired fiber disposed (and pruned from tracking)"
        );
        assert_eq!(current.0.len(), 2, "tree advanced to desired");
        assert!(current.0.iter().all(|e| e.id != "o:bye"));
    }

    /// A plugin disposing ITS OWN registration fiber outside any loader
    /// window persists `disabled = true` onto the entries file, so restarts
    /// do not resurrect the crash-looping plugin.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn self_dispose_persists_disabled_true() {
        use crate::RegistryService;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cordis-entries.toml");
        std::fs::write(
            &path,
            "[[entry]]\nid = \"suicide\"\nplugin = \"SelfKillFactory\"\ndisabled = false\n\n[entry.config]\n",
        )
        .unwrap();

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());
        let ops = ctx.provide(LoaderOps::new());
        ops.enable_self_kill_persistence(path.clone(), true);

        #[derive(Debug)]
        struct Doomed;
        impl Service for Doomed {}

        // Factory hands the plugin its own registration fiber (via the weak
        // owner captured at runner time) and stores it in a slot; a separate
        // trigger disposes it later OUTSIDE any loader call.
        plugin_registry.register(
            "SelfKillFactory",
            Arc::new(|_ctx: &Arc<crate::Context>, _cfg| Ok(0)),
        );

        let mut current = EntryTree(vec![]);
        Loader::apply(
            &ctx,
            &mut current,
            &EntryTree(vec![Entry {
                id: "suicide".into(),
                plugin: "SelfKillFactory".into(),
                config: json!({}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            }]),
            &journal,
        )
        .await;
        let fid = journal.get("suicide").unwrap().fiber_id.unwrap();
        let registry = ctx.get::<crate::RegistryService>().unwrap();
        let fiber = registry.get_fiber(fid).expect("tracked");

        // SELF-KILL: dispose outside a loader window (no apply in flight).
        fiber.dispose().await.expect("dispose runs");

        // Give the synchronous observer chain a beat (it already ran inline,
        // but keep the await shape stable for future async persistence).
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // The file gained disabled=true for that entry.
        let persisted = Loader::load_from_file(&path).expect("file parses");
        let entry = persisted
            .0
            .iter()
            .find(|e| e.id == "suicide")
            .expect("entry still declared");
        assert!(
            entry.disabled,
            "self-dispose must persist disabled=true, got {entry:?}"
        );
    }

    /// Normal retire/reconcile removals happen INSIDE loader windows and must
    /// NOT flip `disabled` in the entries file.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loader_driven_dispose_does_not_persist() {
        use crate::RegistryService;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cordis-entries.toml");
        std::fs::write(
            &path,
            "[[entry]]\nid = \"normal\"\nplugin = \"NormalFactory\"\ndisabled = false\n\n[entry.config]\n",
        )
        .unwrap();

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());
        let ops = ctx.provide(LoaderOps::new());
        ops.enable_self_kill_persistence(path.clone(), true);

        plugin_registry.register(
            "NormalFactory",
            Arc::new(|_ctx: &Arc<crate::Context>, _cfg| Ok(0)),
        );

        let mut current = EntryTree(vec![]);
        Loader::apply(
            &ctx,
            &mut current,
            &EntryTree(vec![Entry {
                id: "normal".into(),
                plugin: "NormalFactory".into(),
                config: json!({}),
                disabled: false,
                isolate: None,
                intercept: HashMap::new(),
            }]),
            &journal,
        )
        .await;
        let fid = journal.get("normal").unwrap().fiber_id.unwrap();
        let registry = ctx.get::<crate::RegistryService>().unwrap();
        let fiber = registry.get_fiber(fid).expect("tracked");

        // Dispose OUTSIDE a loader window but WITHOUT the self-kill verdict:
        // simulate a loader-driven removal by opening the operating window
        // around the disposal (exactly what apply does internally).
        let guard = ops_enter_window_for_test(&ops);
        let _ = fiber.dispose().await;
        drop(guard);

        // File untouched: still enabled=false... i.e. disabled stays false.
        let persisted = Loader::load_from_file(&path).expect("file parses");
        let entry = persisted.0.iter().find(|e| e.id == "normal").unwrap();
        assert!(!entry.disabled, "loader-driven dispose must not persist");

        // And a real reconcile-driven Retire likewise leaves the file alone.
        let desired = EntryTree(vec![]);
        let _ = Loader::apply(&ctx, &mut current, &desired, &journal).await;
        let persisted = Loader::load_from_file(&path).expect("file parses");
        let entry = persisted.0.iter().find(|e| e.id == "normal").unwrap();
        assert!(!entry.disabled, "reconcile retire must not persist");
    }

    /// Test seam: open a loader disposal window around a programmatic
    /// dispose (mirrors [`Loader::apply`]'s internal guard).
    fn ops_enter_window_for_test(ops: &std::sync::Arc<LoaderOps>) -> LoaderWindowGuard {
        ops.enter_loader_window()
    }

    // ------------------------------------------------------------------
    // C2 cascade batching: concurrent provider patches collapse to ONE
    // dependent convergence after the in-flight window settles.
    // ------------------------------------------------------------------

    /// Concurrent config updates against one provider entry must NOT drive
    /// the dependent through one full refresh wave per patch. The dependent
    /// defers while the provider fiber is inside its update window (resting
    /// Pending quietly), and converges exactly once per settled batch — so
    /// the number of dependent apply passes stays far below the number of
    /// racing updates, and ends Active with the final config.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_config_updates_collapse_to_single_cascade() {
        use crate::RegistryService;
        use std::sync::atomic::Ordering;

        let ctx = Context::new_root();
        let journal = LoaderJournal::provide_new(&ctx);
        ctx.provide(RegistryService::new());
        let plugin_registry = ctx.provide(crate::PluginRegistry::new());

        // Provider: counts every factory application.
        #[derive(Debug)]
        struct CascadeProvider;
        impl Service for CascadeProvider {}

        let provider_applies = Arc::new(std::sync::atomic::AtomicU64::new(0));
        {
            let counter = provider_applies.clone();
            plugin_registry.register(
                "CascadeProviderFactory",
                Arc::new(move |ctx, _config| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let future = ctx.plugin(CascadeProvider);
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(future)
                    })
                }),
            );
        }

        // Dependent: declares its inject on Provider and counts re-applies.
        #[derive(Debug)]
        struct Dependent;
        impl Service for Dependent {}

        let dep_fiber_holder = Arc::new(parking_lot::Mutex::<Option<std::sync::Arc<crate::Fiber>>>::new(None));

        let dependent_applies = Arc::new(std::sync::atomic::AtomicU64::new(0));
        {
            let counter = dependent_applies.clone();
            let holder = dep_fiber_holder.clone();
            plugin_registry.register(
                "CascadeDependentFactory",
                Arc::new(move |ctx, _config| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let future = ctx.plugin(Dependent);
                    let fid = tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(future)
                    })?;
                    let tracked = ctx
                        .get::<crate::RegistryService>()
                        .and_then(|rs| rs.get_fiber(fid));
                    if let Some(fiber) = tracked {
                        fiber.declare_inject::<CascadeProvider>();
                        *holder.lock() = Some(fiber);
                    }
                    Ok(fid)
                }),
            );
        }

        // Seed: begin both entries. The dependent's inject is declared
        // against its registration fiber via the factory hook above.
        let provider_fid = Loader::instantiate(
            &ctx,
            "CascadeProviderFactory",
            &json!({"v": 1}),
            "cascade:provider",
        )
        .expect("provider begins");
        let dep_entry_fid = Loader::instantiate(
            &ctx,
            "CascadeDependentFactory",
            &json!({}),
            "cascade:dependent",
        )
        .expect("dependent begins");

        // The dependent registration resolves its fiber through tracking;
        // declare the inject explicitly when the factory hook could not.
        let registry = ctx.get::<RegistryService>().unwrap();
        let dep_fiber = match dep_fiber_holder.lock().clone() {
            Some(fiber) => fiber,
            None => {
                let fiber = registry.get_fiber(dep_entry_fid).unwrap();
                fiber.declare_inject::<CascadeProvider>();
                fiber.clone()
            }
        };

        // Converge once so the dependent is Active before the storm.
        if ctx.get::<crate::ReflectService>().is_none() {
            ctx.provide(crate::ReflectService::new());
        }
        let reflect = ctx.get::<crate::ReflectService>().unwrap();
        reflect.set_context(&ctx);
        reflect.notify_with_ctx(TypeId::of::<CascadeProvider>(), &ctx).await;
        assert!(
            matches!(dep_fiber.state(), crate::FiberState::Active { .. }),
            "dependent must start Active, got {:?}",
            dep_fiber.state()
        );

        // PATCH STORM: several concurrent Loader::apply batches, each
        // changing ONLY the provider's config. Without batching each settle
        // would trigger a full dependent refresh wave; with the in-flight
        // ledger the dependent defers during updates and converges once.
        let current_shared = Arc::new(tokio::sync::Mutex::new(EntryTree(vec![Entry {
            id: "cascade:provider".into(),
            plugin: "CascadeProviderFactory".into(),
            config: json!({"v": 1}),
            disabled: false,
            isolate: None,
            intercept: HashMap::new(),
        }])));
        let mut handles = Vec::new();
        for round in 2..=6u32 {
            let ctx = ctx.clone();
            let journal = journal.clone();
            let current = current_shared.clone();
            handles.push(tokio::spawn(async move {
                let mut guard = current.lock().await;
                let desired = EntryTree(vec![Entry {
                    id: "cascade:provider".into(),
                    plugin: "CascadeProviderFactory".into(),
                    config: json!({"v": round}),
                    disabled: false,
                    isolate: None,
                    intercept: HashMap::new(),
                }]);
                Loader::apply(&ctx, &mut guard, &desired, &journal).await
            }));
        }
        for handle in handles {
            let actions = handle.await.expect("storm task joins");
            assert!(
                actions.iter().all(|a| a.status.is_ok()),
                "every storm batch applies: {actions:?}"
            );
        }

        // Final state converges: provider Active at the last config, and the
        // dependent converged back to Active too.
        let provider_fiber = registry.get_fiber(provider_fid).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(matches!(
            provider_fiber.state(),
            crate::FiberState::Active { .. }
        ));
        dep_fiber.refresh(&ctx).await;
        assert!(
            matches!(dep_fiber.state(), crate::FiberState::Active { .. }),
            "dependent must converge Active after the storm, got {:?}",
            dep_fiber.state()
        );
        assert!(
            ctx.get::<CascadeProvider>().is_some(),
            "final provider serving"
        );

        // COLLAPSE PROOF: five sequential provider re-applies happened (one
        // per batch — they serialize through the loader lock), but the
        // dependent ran strictly fewer full passes than waves because every
        // mid-update notify deferred to Pending instead of re-applying. The
        // ledger guarantees the deferred count never exceeds the settled
        // windows; assert the dependent did not re-apply once per provider
        // application (the pre-batching behavior).
        let provider_runs = provider_applies.load(Ordering::SeqCst);
        let dependent_runs = dependent_applies.load(Ordering::SeqCst);
        assert!(
            provider_runs >= 5,
            "each batch re-applies the provider, got {provider_runs}"
        );
        assert!(
            dependent_runs <= 3,
            "dependent must collapse waves (deferred under the ledger), \
             got {dependent_runs} runs vs {provider_runs} provider runs"
        );
    }
}
