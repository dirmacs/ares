//! Explicit module dependency graph beside the file-watch / HMR machinery.
//!
//! [`crate::watcher`] already fans changes out to *service-level* dependents:
//! the debounced batch notifies [`crate::ReflectService`], which BFS-walks the
//! `TypeId` dependents and refreshes fibers. That layer answers "which fibers
//! consume this service type?" — it cannot answer "which *plugin* must reload
//! because this file changed?", because file/plugin edges carry no TypeId.
//!
//! [`ModuleGraph`] is that missing edge layer: callers register every dynamic
//! module under a key (typically the watched file stem or module URL) together
//! with the keys it depends on and the plugin that implements it. When the
//! watcher settles a batch, it maps each changed path to its file stem and
//! hands the keys to [`ModuleGraph::change_many`], which
//!
//! 1. computes the **transitive** affected plugin set BEFORE mutating anything
//!    ([`ModuleGraph::depends_on`] DFS with a visited set, so dependency
//!    cycles terminate instead of spinning),
//! 2. dedupes repeated input keys so every affected plugin reloads EXACTLY
//!    ONCE per transaction,
//! 3. applies sequentially through the [`ModuleReload`] seam, rolling the
//!    failing plugin back to its previous state on the first error while the
//!    successful siblings stay Active,
//! 4. classifies the transaction: an input batch where NO key matches a
//!    registered module is [`ChangeOutcome::Ignored`] (external/unknown
//!    noise), otherwise [`ChangeOutcome::Reloaded`] names the plugins or
//!    [`ChangeOutcome::RolledBack`] names the failure.
//!
//! Without a registered [`ModuleGraph`] on the context the watcher skips the
//! layer entirely — zero cost for deployments that only need the TypeId path.
//! The HMR dlopen fingerprint gate ([`crate::hmr`]) is untouched by this
//! layer; dylib applies continue to happen before graph fan-out ordering
//! concerns, and neither consults the other.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::service::CordisError;

/// One registered module: where it came from, what it consumes, which plugin
/// implements it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleEntry {
    /// Keys this module depends on. When any of them changes (directly or
    /// transitively), this module's plugin is part of the affected set.
    pub dependencies: Vec<String>,
    /// Plugin that implements the module; the name handed to [`ModuleReload`].
    pub plugin_name: String,
}

/// Apply seam between the graph and whatever actually re-instantiates a
/// plugin (loader re-apply, HMR swap, test double).
///
/// `reload` brings `plugin` to its new state; `rollback` re-registers the
/// PREVIOUS state after a failed `reload`, restoring the last-known-good
/// configuration. Implementations must be idempotent per call site — the
/// graph invokes each exactly once per affected plugin per transaction.
pub trait ModuleReload: Send + Sync + 'static {
    /// Reload `plugin` to reflect the settled change batch.
    fn reload(&self, ctx: &Arc<crate::Context>, plugin: &str) -> Result<(), CordisError>;

    /// Re-register the plugin's previous state after a failed reload.
    fn rollback(&self, ctx: &Arc<crate::Context>, plugin: &str) -> Result<(), CordisError>;
}

/// Default seam: log-only, never fails. Deployments that only want the
/// classification/propagation logic wire their own [`ModuleReload`].
pub struct NoopReload;

impl ModuleReload for NoopReload {
    fn reload(&self, _ctx: &Arc<crate::Context>, plugin: &str) -> Result<(), CordisError> {
        tracing::debug!(plugin = %plugin, "module-graph noop reload");
        Ok(())
    }

    fn rollback(&self, _ctx: &Arc<crate::Context>, plugin: &str) -> Result<(), CordisError> {
        tracing::debug!(plugin = %plugin, "module-graph noop rollback");
        Ok(())
    }
}

/// Classified result of one [`ModuleGraph::change_many`] transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeOutcome {
    /// No input key matched a registered module — external/unknown noise.
    /// Nothing was computed against the applier and nothing changed.
    Ignored,
    /// Every affected plugin reloaded; list is deduped and follows
    /// breadth-first propagation order from the input keys.
    Reloaded(Vec<String>),
    /// Sequential apply stopped at the first failure. `reloaded` siblings
    /// stay Active; the failing plugin was rolled back to its previous state.
    /// `error` carries the reload failure text (plus rollback status when the
    /// rollback itself also failed).
    RolledBack {
        reloaded: Vec<String>,
        failed_plugin: String,
        error: String,
    },
}

impl ChangeOutcome {
    /// One-line rendering for logs and settle-barrier style reporting.
    pub fn summary(&self) -> String {
        match self {
            ChangeOutcome::Ignored => "ignored (no registered module matched)".to_string(),
            ChangeOutcome::Reloaded(plugins) => format!("reloaded [{}]", plugins.join(", ")),
            ChangeOutcome::RolledBack {
                reloaded,
                failed_plugin,
                error,
            } => format!(
                "rolled back {} after [{}] applied: {error}",
                failed_plugin,
                reloaded.join(", ")
            ),
        }
    }
}

impl std::fmt::Display for ChangeOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary())
    }
}

/// Explicit file/plugin dependency graph over registered modules.
///
/// Keys are typically the watched file stem (`agents/foo.toon` → `"foo"`) or
/// the module URL; edges point from a module to the keys it declares in
/// `dependencies`. Storage is a `BTreeMap`, so propagation order is
/// deterministic across runs regardless of registration order.
pub struct ModuleGraph {
    modules: parking_lot::RwLock<BTreeMap<String, ModuleEntry>>,
    reloader: parking_lot::RwLock<Arc<dyn ModuleReload>>,
}

impl Default for ModuleGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::Service for ModuleGraph {}

impl ModuleGraph {
    pub fn new() -> Self {
        Self::with_reloader(Arc::new(NoopReload))
    }

    /// Graph wired to a concrete apply seam.
    pub fn with_reloader(reloader: Arc<dyn ModuleReload>) -> Self {
        Self {
            modules: parking_lot::RwLock::new(BTreeMap::new()),
            reloader: parking_lot::RwLock::new(reloader),
        }
    }

    /// Swap the apply seam (e.g. install the production reloader after boot).
    pub fn set_reloader(&self, reloader: Arc<dyn ModuleReload>) {
        *self.reloader.write() = reloader;
    }

    /// Register (or replace) module `key` — the URL/file-stem identifier —
    /// with its declared `dependencies` and implementing `plugin_name`.
    pub fn register_module(
        &self,
        key: impl Into<String>,
        dependencies: Vec<String>,
        plugin_name: impl Into<String>,
    ) {
        self.modules.write().insert(
            key.into(),
            ModuleEntry {
                dependencies,
                plugin_name: plugin_name.into(),
            },
        );
    }

    /// Snapshot of one module entry, if registered.
    pub fn get(&self, key: &str) -> Option<ModuleEntry> {
        self.modules.read().get(key).cloned()
    }

    /// All registered module keys, ascending.
    pub fn module_keys(&self) -> Vec<String> {
        self.modules.read().keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.modules.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.modules.read().is_empty()
    }

    /// Transitive dependents of `key`, INCLUDING `key` itself, in
    /// breadth-first propagation order.
    ///
    /// The DFS carries a visited set, so cyclic declarations (`A` depends on
    /// `B`, `B` depends on `A`) terminate after visiting each module once
    /// while still propagating to everything reachable around the cycle.
    pub fn depends_on(&self, key: &str) -> Vec<String> {
        let modules = self.modules.read();
        let mut visited = HashSet::new();
        let mut order = Vec::new();
        let mut queue = VecDeque::new();
        if modules.contains_key(key) {
            visited.insert(key.to_string());
            queue.push_back(key.to_string());
        }
        while let Some(current) = queue.pop_front() {
            order.push(current.clone());
            for dependent in reverse_edges(&modules, &current) {
                if visited.insert(dependent.clone()) {
                    queue.push_back(dependent);
                }
            }
        }
        order
    }

    /// Transaction over one settled batch of changed keys.
    ///
    /// Phase 1 (read-only) computes the transitive affected plugin set across
    /// ALL input keys — shared visited set, so a plugin reachable from several
    /// inputs appears exactly once. If no input key matches a registered
    /// module the transaction ends [`ChangeOutcome::Ignored`] before any
    /// mutation. Phase 2 applies the affected plugins sequentially; the first
    /// failure rolls that plugin back to its previous state and stops the
    /// batch, leaving earlier successes Active and reporting
    /// [`ChangeOutcome::RolledBack`].
    pub fn change_many(&self, ctx: &Arc<crate::Context>, keys: &[String]) -> ChangeOutcome {
        // Phase 1: compute the full affected set BEFORE mutating anything.
        let affected = {
            let modules = self.modules.read();
            if !keys.iter().any(|k| modules.contains_key(k)) {
                return ChangeOutcome::Ignored;
            }
            let mut visited: HashSet<String> = HashSet::new();
            let mut queue: VecDeque<String> = VecDeque::new();
            for key in keys {
                if modules.contains_key(key) && visited.insert(key.clone()) {
                    queue.push_back(key.clone());
                }
            }
            let mut plugins: Vec<String> = Vec::new();
            let mut seen_plugins: HashSet<String> = HashSet::new();
            while let Some(current) = queue.pop_front() {
                if let Some(entry) = modules.get(&current) {
                    if seen_plugins.insert(entry.plugin_name.clone()) {
                        plugins.push(entry.plugin_name.clone());
                    }
                }
                for dependent in reverse_edges(&modules, &current) {
                    if visited.insert(dependent.clone()) {
                        queue.push_back(dependent);
                    }
                }
            }
            plugins
        };

        // Phase 2: sequential apply with rollback-on-first-failure.
        let reloader = self.reloader.read().clone();
        let mut reloaded: Vec<String> = Vec::with_capacity(affected.len());
        for plugin in affected {
            match reloader.reload(ctx, &plugin) {
                Ok(()) => reloaded.push(plugin),
                Err(err) => {
                    let rollback_err = reloader.rollback(ctx, &plugin).err();
                    let error = match rollback_err {
                        Some(rb) => format!("{err}; ROLLBACK ALSO FAILED for {plugin}: {rb}"),
                        None => format!("{err}; rolled back {plugin} to its previous state"),
                    };
                    tracing::error!(
                        plugin = %plugin,
                        applied = ?reloaded,
                        %error,
                        "module-graph change_many aborted"
                    );
                    return ChangeOutcome::RolledBack {
                        reloaded,
                        failed_plugin: plugin,
                        error,
                    };
                }
            }
        }
        ChangeOutcome::Reloaded(reloaded)
    }
}

/// Modules whose `dependencies` contain `target`, in ascending key order
/// (deterministic because storage is a `BTreeMap`).
fn reverse_edges(modules: &BTreeMap<String, ModuleEntry>, target: &str) -> Vec<String> {
    modules
        .iter()
        .filter(|(_, entry)| entry.dependencies.iter().any(|d| d == target))
        .map(|(key, _)| key.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Context;

    /// Counting fake: records every op, optionally fails named plugins.
    struct FakeReload {
        fail_on: Vec<String>,
        ops: parking_lot::Mutex<Vec<String>>,
    }

    impl FakeReload {
        fn new(fail_on: &[&str]) -> Self {
            Self {
                fail_on: fail_on.iter().map(|s| s.to_string()).collect(),
                ops: parking_lot::Mutex::new(Vec::new()),
            }
        }

        fn ops(&self) -> Vec<String> {
            self.ops.lock().clone()
        }
    }

    impl ModuleReload for FakeReload {
        fn reload(&self, _ctx: &Arc<Context>, plugin: &str) -> Result<(), CordisError> {
            self.ops.lock().push(format!("reload:{plugin}"));
            if self.fail_on.iter().any(|f| f == plugin) {
                Err(CordisError::Fiber(format!("{plugin} failed to rebuild")))
            } else {
                Ok(())
            }
        }

        fn rollback(&self, _ctx: &Arc<Context>, plugin: &str) -> Result<(), CordisError> {
            self.ops.lock().push(format!("rollback:{plugin}"));
            Ok(())
        }
    }

    fn ctx() -> Arc<Context> {
        Context::new_root()
    }

    #[tokio::test]
    async fn dependency_change_reloads_dependents_transitively() {
        // chain: a <- b <- c (c depends on b, b depends on a)
        let fake = Arc::new(FakeReload::new(&[]));
        let graph = ModuleGraph::with_reloader(fake.clone());
        graph.register_module("a", vec![], "P.a");
        graph.register_module("b", vec!["a".into()], "P.b");
        graph.register_module("c", vec!["b".into()], "P.c");

        assert_eq!(graph.depends_on("a"), vec!["a", "b", "c"]);

        let outcome = graph.change_many(&ctx(), &["a".to_string()]);
        assert_eq!(outcome, ChangeOutcome::Reloaded(s(&["P.a", "P.b", "P.c"])));
        // Exactly one reload per plugin, propagation order.
        assert_eq!(fake.ops(), vec!["reload:P.a", "reload:P.b", "reload:P.c"]);
    }

    #[tokio::test]
    async fn cycles_terminate_and_still_propagate() {
        // m1 <-> m2 cycle, with m3 hanging off m1 outside the cycle.
        let fake = Arc::new(FakeReload::new(&[]));
        let graph = ModuleGraph::with_reloader(fake.clone());
        graph.register_module("m1", vec!["m2".into()], "P.1");
        graph.register_module("m2", vec!["m1".into()], "P.2");
        graph.register_module("m3", vec!["m1".into()], "P.3");

        // DFS terminates despite the cycle and still reaches m3.
        assert_eq!(graph.depends_on("m2"), vec!["m2", "m1", "m3"]);

        let outcome = graph.change_many(&ctx(), &["m2".to_string()]);
        assert_eq!(outcome, ChangeOutcome::Reloaded(s(&["P.2", "P.1", "P.3"])));
        // Each cycle member reloaded exactly once — no infinite loop, no dupes.
        assert_eq!(fake.ops(), vec!["reload:P.2", "reload:P.1", "reload:P.3"]);
    }

    #[tokio::test]
    async fn batched_changes_reload_each_plugin_once() {
        // x feeds y and z; y also feeds z; w unrelated. Batch repeats inputs.
        let fake = Arc::new(FakeReload::new(&[]));
        let graph = ModuleGraph::with_reloader(fake.clone());
        graph.register_module("x", vec![], "P.x");
        graph.register_module("y", vec!["x".into()], "P.y");
        graph.register_module("z", vec!["x".into(), "y".into()], "P.z");
        graph.register_module("w", vec![], "P.w");

        let keys = vec!["x".to_string(), "x".to_string(), "y".to_string()];
        let outcome = graph.change_many(&ctx(), &keys);
        assert_eq!(outcome, ChangeOutcome::Reloaded(s(&["P.x", "P.y", "P.z"])));
        // z reachable from BOTH x and y reloads EXACTLY ONCE; w untouched.
        assert_eq!(fake.ops(), vec!["reload:P.x", "reload:P.y", "reload:P.z"]);
    }

    #[tokio::test]
    async fn rollback_keeps_successful_siblings_active() {
        let fake = Arc::new(FakeReload::new(&["P.b"]));
        let graph = ModuleGraph::with_reloader(fake.clone());
        graph.register_module("a", vec![], "P.a");
        graph.register_module("b", vec!["a".into()], "P.b");
        graph.register_module("c", vec!["b".into()], "P.c");

        let outcome = graph.change_many(&ctx(), &["a".to_string()]);
        match outcome {
            ChangeOutcome::RolledBack {
                reloaded,
                failed_plugin,
                error,
            } => {
                // Successful sibling stayed applied (never rolled back).
                assert_eq!(reloaded, vec!["P.a"]);
                assert_eq!(failed_plugin, "P.b");
                assert!(error.contains("P.b failed to rebuild"));
                assert!(error.contains("rolled back"));
            }
            other => panic!("expected RolledBack, got {other:?}"),
        }
        // Sequence proves: A applied and left alone, B attempted then restored,
        // C never attempted (first-failure stop).
        assert_eq!(
            fake.ops(),
            vec!["reload:P.a", "reload:P.b", "rollback:P.b",]
        );
    }

    #[tokio::test]
    async fn external_key_classified_ignored() {
        let fake = Arc::new(FakeReload::new(&[]));
        let graph = ModuleGraph::with_reloader(fake.clone());
        graph.register_module("known", vec![], "P.known");

        // Pure-external batch: nothing matches a registered module.
        let keys = vec!["external-thing".to_string(), "also-unknown".to_string()];
        assert_eq!(graph.change_many(&ctx(), &keys), ChangeOutcome::Ignored);
        assert!(fake.ops().is_empty());

        // Empty batch likewise touches nothing.
        assert_eq!(graph.change_many(&ctx(), &[]), ChangeOutcome::Ignored);

        // Mixed batch: the known key still drives its transaction…
        let mixed = vec!["external-thing".to_string(), "known".to_string()];
        assert_eq!(
            graph.change_many(&ctx(), &mixed),
            ChangeOutcome::Reloaded(s(&["P.known"]))
        );
    }

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| i.to_string()).collect()
    }
}
