//! Classified entries reload shared by the file watcher and the admin
//! endpoints.
//!
//! `reload_entries_from_disk` hoists the parse → compose → diff-apply →
//! classify flow that previously lived duplicated in the server binary and
//! the admin handler. It returns a [`ReloadOutcome`] instead of discarding
//! per-action results, and holds a process-wide [`RELOAD_LOCK`] across
//! parse → apply → write-back so a watcher batch and an admin reload can
//! never interleave (closing the `CurrentEntries` lost-update race).

use std::path::Path;
use std::sync::{Arc, LazyLock};

use crate::stamp::ReloadOutcome;

/// Process-wide serialization for entries reloads.
///
/// Held across parse → apply → write-back inside
/// [`reload_entries_from_disk`]; watcher batches and admin reloads take it
/// before touching `CurrentEntries`, so batches cannot interleave and lose
/// updates. A plain async mutex: holders may `.await` while applying.
static RELOAD_LOCK: LazyLock<Arc<tokio::sync::Mutex<()>>> =
    LazyLock::new(|| Arc::new(tokio::sync::Mutex::new(())));

fn reload_lock() -> Arc<tokio::sync::Mutex<()>> {
    Arc::clone(&*RELOAD_LOCK)
}

/// Reload the Cordis entries program from disk against the provided loader
/// state, returning the classified outcome.
///
/// Flow:
/// 1. Take the process-wide reload lock (watcher batches and admin reloads
///    serialize; no interleaved lost updates on `CurrentEntries`).
/// 2. Parse + compose the file at `entries_path` (`@include` splices,
///    `@group` flattening, `${rhai: …}` interpolation). Composition is
///    fail-open exactly like boot: on error the raw re-parse is used, so a
///    bad include cannot brick hot-reload.
/// 3. Diff-apply through [`crate::loader::Loader::reload_current`] against
///    the last applied tree.
/// 4. Write the reconciled tree back into `CurrentEntries` only when at
///    least one action ran.
/// 5. Classify into [`ReloadOutcome`] (`NoChange` / `Applied` / `Failed`).
///
/// Missing loader state ([`crate::LoaderJournal`] /
/// [`crate::loader::CurrentEntries`]) yields `Failed` — those deployments
/// have no file program to reload.
pub async fn reload_entries_from_disk(
    ctx: &Arc<crate::Context>,
    entries_path: &Path,
) -> ReloadOutcome {
    use crate::{compose_all, Loader};

    let lock = reload_lock();
    let _guard = lock.lock().await;

    let Some(journal) = ctx.get::<crate::LoaderJournal>() else {
        return ReloadOutcome::Failed {
            error: "LoaderJournal is not provided on this context".to_string(),
        };
    };
    let Some(current_entries) = ctx.get::<crate::loader::CurrentEntries>() else {
        return ReloadOutcome::Failed {
            error: "CurrentEntries is not provided on this context".to_string(),
        };
    };

    // Compose the freshly re-read tree BEFORE diffing so `@include` splices,
    // `@group` flattening, and `${rhai: …}` interpolation match boot state
    // (fail-open: on composition error we proceed with the raw re-parse).
    let desired = match Loader::load_from_file(entries_path) {
        Ok(mut tree) => {
            let base_dir = entries_path.parent().unwrap_or_else(|| Path::new("."));
            if let Err(e) = compose_all(&mut tree.0, base_dir) {
                tracing::error!(
                    path = %entries_path.display(),
                    error = %e,
                    "Cordis compose failed on reload; proceeding with raw entries"
                );
                tree = Loader::load_from_file(entries_path).unwrap_or_default();
            }
            tree
        }
        Err(e) => {
            tracing::warn!(path = %entries_path.display(), error = %e,
                "Cordis hot-reload: reparse failed after change");
            return ReloadOutcome::Failed {
                error: e.to_string(),
            };
        }
    };

    let mut current = current_entries.tree.lock().expect("entries lock").clone();
    let actions = Loader::reload_current(ctx, entries_path, &mut current, &desired, &journal).await;
    // reload_current returns None only when the caller-supplied parse already
    // failed; we parsed above, so this is unreachable in practice — treat as
    // NoChange defensively rather than panicking a watcher thread.
    let Some(actions) = actions else {
        return ReloadOutcome::NoChange;
    };

    for action in &actions {
        match &action.status {
            Ok(()) => tracing::info!(
                entry_id = %action.id,
                action = action.action,
                "Cordis hot-reload: applied"
            ),
            Err(err) => tracing::warn!(
                entry_id = %action.id,
                action = action.action,
                error = %err,
                "Cordis hot-reload: action failed"
            ),
        }
    }

    if actions.is_empty() {
        return ReloadOutcome::NoChange;
    }
    *current_entries.tree.lock().expect("entries lock") = current;
    tracing::info!(
        actions = actions.len(),
        "Cordis hot-reload: reconciled entries change"
    );
    ReloadOutcome::classify(&actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::loader::EntryTree;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Minimal factory providing one Probe service; instance value mirrors
    /// the entry config's `v` so tests observe which generation is live.
    fn probe_registry(registry: &crate::PluginRegistry) -> Arc<AtomicU64> {
        let live = Arc::new(AtomicU64::new(0));
        let live_cb = live.clone();
        registry.register(
            "ProbeService",
            Arc::new(move |ctx, cfg| {
                let v = cfg.get("v").and_then(|x| x.as_u64()).unwrap_or(1);
                let probe = Probe(AtomicU64::new(v));
                let live_cb = live_cb.clone();
                let fut = async move {
                    ctx.plugin(probe).await?;
                    live_cb.store(v, Ordering::SeqCst);
                    Ok(0u64)
                };
                tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
            }),
        );
        live
    }

    #[derive(Debug)]
    struct Probe(pub AtomicU64);
    impl crate::Service for Probe {}

    const PROBE_TOML: &str =
        "[[entry]]\nid = \"probe\"\nplugin = \"ProbeService\"\ndisabled = false\n\n[entry.config]\nv = 1\n";

    /// Full fixture: loader context + temp TOML program seeded with
    /// `initial_toml`, `CurrentEntries` starting from `current`.
    fn build_fixture(
        tag: &str,
        initial_toml: &str,
        current: EntryTree,
    ) -> (Arc<Context>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("cordis-hmr-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let path = dir.join("cordis-entries.toml");
        std::fs::write(&path, initial_toml).expect("seed entries file");

        let ctx = Context::new_root();
        ctx.provide(crate::ReflectService::new());
        crate::LoaderJournal::provide_new(&ctx);
        ctx.provide(crate::RegistryService::new());
        let registry = ctx.provide(crate::PluginRegistry::new());
        let _live = probe_registry(&registry);

        ctx.provide_arc(Arc::new(crate::loader::CurrentEntries {
            tree: Arc::new(std::sync::Mutex::new(current)),
            path: path.clone(),
        }));
        (ctx, path)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_reload_serialized() {
        // Two concurrent reloads over the same workspace must serialize on
        // RELOAD_LOCK and converge to ONE final state: the journal/tree end
        // consistent with the last-applied file content, no panic, both Ok.
        let (ctx, path) = build_fixture("serial", "", EntryTree(vec![]));

        // File gains one entry; two racing reloads must both see it applied
        // exactly once in aggregate (the second observes an empty diff).
        std::fs::write(&path, PROBE_TOML).expect("write v2");

        let ctx_a = ctx.clone();
        let path_a = path.clone();
        let ctx_b = ctx.clone();
        let path_b = path.clone();
        let (ra, rb) = tokio::join!(
            reload_entries_from_disk(&ctx_a, &path_a),
            reload_entries_from_disk(&ctx_b, &path_b),
        );

        assert!(!matches!(ra, ReloadOutcome::Failed { .. }), "a: {ra:?}");
        assert!(!matches!(rb, ReloadOutcome::Failed { .. }), "b: {rb:?}");
        // Exactly one Applied (first writer) and one NoChange (empty second
        // diff) in either order proves serialization without double-apply.
        let applied_count = [ra.summary(), rb.summary()]
            .iter()
            .filter(|s| s.starts_with("applied"))
            .count();
        assert_eq!(
            applied_count, 1,
            "one Applied + one NoChange under the lock, got {ra:?} / {rb:?}"
        );

        // Single final state: journal shows the probe journaled once with a
        // tracked fiber; the service is live; current tree matches the file.
        let journal = ctx.get::<crate::LoaderJournal>().unwrap();
        let rec = journal.get("probe").expect("journaled");
        assert!(rec.fiber_id.is_some(), "tracked fiber recorded");
        assert!(
            ctx.get::<Probe>().is_some(),
            "probe instantiated exactly once overall"
        );
        let current_entries = ctx.get::<crate::loader::CurrentEntries>().unwrap();
        let tree = current_entries.tree.lock().expect("entries lock");
        assert_eq!(tree.0.len(), 1, "final tree matches the file");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reload_reports_missing_state_as_failed() {
        // Library-style context without CurrentEntries: classified failure,
        // never a panic.
        let ctx = Context::new_root();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cordis-entries.toml");
        std::fs::write(&path, PROBE_TOML).unwrap();
        match reload_entries_from_disk(&ctx, &path).await {
            ReloadOutcome::Failed { error } => {
                assert!(error.contains("provided"), "error names state: {error}")
            }
            other => panic!("missing state must be Failed, got {other:?}"),
        }
    }
}
