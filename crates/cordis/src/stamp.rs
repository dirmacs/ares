//! Content-hash change stamps for hot-reload decisions.
//!
//! [`FileStamp`] answers "did this file actually change?" without re-reading
//! it into config: a cheap length + content-hash pair. Watchers compare the
//! stamp captured at last load against the stamp of the file on disk, so
//! editor churn (touch, partial writes, mtime-only noise) that leaves bytes
//! identical no longer triggers a reload cycle.
//!
//! ## [`ReloadOutcome`] — classified result of one reload cycle
//!
//! The reload pipeline computes rich results that used to be thrown away at
//! the watcher boundary: `Loader::apply` / `Loader::reload_current` build
//! `Vec<AppliedAction { action, status, verified }>` (see `loader.rs`) but
//! callers only learned "reload ran". [`ReloadOutcome`] carries the
//! classification instead:
//!
//! - [`ReloadOutcome::NoChange`] — nothing to do (empty diff); previous
//!   state intact. Stamp-gated short-circuits never reach classification.
//! - [`ReloadOutcome::Applied`] — every action applied `Ok`; carries the
//!   actions for logging/routing.
//! - [`ReloadOutcome::Failed`] — parse failure, or any action reporting
//!   `Err`; carries joined error text naming every failing entry.
//!   Partially-applied batches still report `Failed`; whatever succeeded
//!   before the failure stays applied (loader semantics).
//!
//! [`crate::reload::reload_entries_from_disk`] produces the outcome; the
//! watcher boundary (`watcher.rs`) carries it through the `WatchOnChange`
//! callback and a settle barrier so admin readers observe settled state
//! instead of racing an in-flight batch.
//!
//! - The watcher thread stamps the entries file before dispatch; on
//!   `matches()` → emit `NoChange` without touching the loader.
//! - Otherwise run apply/reload and classify the returned
//!   `Vec<AppliedAction>` into `Applied` (each action rendered
//!   `"RebuildFiber tool:calc (verified)"` style) or `Failed` on any
//!   non-applied status.
//! - Carry outcomes through a `WatchEvent::Reload(ReloadOutcome)`-style
//!   callback channel so listeners can log/route them.
//! - A **settle barrier** gates the admin endpoint answer: the HTTP handler
//!   awaits "no reload in flight + quiet window elapsed" before replying,
//!   so a client that just PUT new entries reads back applied state instead
//!   of racing the watcher. Implementation sketch: a shared
//!   `Arc<(Mutex<Option<ReloadOutcome>>, Condvar)>` flipped by the watcher's
//!   `on_quiet` callback (see `worker::watch_plugin_dirs`), awaited with a
//!   bounded timeout by admin handlers.

use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::Path;

/// Above this size only the length participates in change detection
/// (`hash == 0`); hashing arbitrarily large plugin blobs is wasted work when
/// a length mismatch already distinguishes almost all real edits.
const MAX_HASHED_BYTES: u64 = 16 * 1024 * 1024; // 16 MiB

/// Cheap change fingerprint of one file: byte length plus (capped) content
/// hash. Two stamps match only when both fields agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    /// File size in bytes at stamp time.
    pub len: u64,
    /// `DefaultHasher` digest of the first [`MAX_HASHED_BYTES`] bytes,
    /// or `0` for files above the cap (length-only comparison).
    pub hash: u64,
}

impl FileStamp {
    /// Stamp the file at `path`; `None` when it cannot be read (missing,
    /// permission-denied, directory).
    pub fn of_path(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        let len = metadata.len();
        let mut hash = 0u64;
        if len <= MAX_HASHED_BYTES {
            let bytes = std::fs::read(path).ok()?;
            let mut hasher = DefaultHasher::new();
            bytes.hash(&mut hasher);
            hash = hasher.finish();
        }
        Some(Self { len, hash })
    }

    /// Whether `other` describes identical content as far as this stamp can
    /// tell (same length, same capped-content hash).
    pub fn matches(&self, other: &FileStamp) -> bool {
        self.len == other.len && self.hash == other.hash
    }
}

/// Classified result of one entries-reload cycle (see module docs).
#[derive(Debug, Clone)]
pub enum ReloadOutcome {
    /// Nothing to do — the diff produced zero actions; state untouched.
    NoChange,
    /// Every reconcile action applied `Ok`; carries the applied actions.
    Applied {
        actions: Vec<crate::loader::AppliedAction>,
    },
    /// Reparse failed, or at least one action reported `Err`; carries the
    /// joined error text naming every failing entry. Actions that succeeded
    /// before a failure stay applied (loader semantics).
    Failed { error: String },
}

impl ReloadOutcome {
    /// Classify a batch of applied actions: any `Err` status collapses the
    /// whole batch into [`ReloadOutcome::Failed`] carrying the joined error
    /// text; otherwise the batch is [`ReloadOutcome::Applied`].
    ///
    /// An empty slice classifies as [`ReloadOutcome::NoChange`] (no diff).
    pub fn classify(actions: &[crate::loader::AppliedAction]) -> Self {
        if actions.is_empty() {
            return Self::NoChange;
        }
        let errors: Vec<String> = actions
            .iter()
            .filter_map(|a| {
                a.status
                    .as_ref()
                    .err()
                    .map(|e| format!("{} ({}): {}", a.id, a.action, e))
            })
            .collect();
        if errors.is_empty() {
            Self::Applied {
                actions: actions.to_vec(),
            }
        } else {
            Self::Failed {
                error: errors.join("; "),
            }
        }
    }

    /// One-line human summary for logs and admin surfaces.
    pub fn summary(&self) -> String {
        match self {
            Self::NoChange => "no change".to_string(),
            Self::Applied { actions } => {
                let listed: Vec<String> = actions
                    .iter()
                    .map(|a| {
                        if a.verified {
                            format!("{}:{}", a.action, a.id)
                        } else {
                            format!("{}:{} (unverified)", a.action, a.id)
                        }
                    })
                    .collect();
                format!("applied {}: {}", actions.len(), listed.join(", "))
            }
            Self::Failed { error } => format!("failed: {error}"),
        }
    }
}

impl fmt::Display for ReloadOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.summary())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_content_yields_same_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entries.json");
        std::fs::write(&path, b"{\"entry\":[]}\n").unwrap();
        let a = FileStamp::of_path(&path).unwrap();
        let b = FileStamp::of_path(&path).unwrap();
        assert!(a.matches(&b));
        assert_eq!(a, b);
        assert_eq!(a.len, b"{\"entry\":[]}\n".len() as u64);
        assert_ne!(a.hash, 0, "in-cap files must carry a content hash");
    }

    #[test]
    fn equal_length_different_content_yields_different_stamps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entries.json");
        std::fs::write(&path, b"disabled = false").unwrap();
        let before = FileStamp::of_path(&path).unwrap();
        std::fs::write(&path, b"disabled = true ").unwrap();
        let after = FileStamp::of_path(&path).unwrap();
        assert_eq!(before.len, after.len, "test precondition: equal lengths");
        assert!(!before.matches(&after), "content change must flip the hash");
    }

    #[test]
    fn missing_file_stamps_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(FileStamp::of_path(&dir.path().join("absent.toml")), None);
    }

    #[test]
    fn oversized_files_fall_back_to_length_only() {
        let len = MAX_HASHED_BYTES + 1;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.bin");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(len).unwrap();
        let stamp = FileStamp::of_path(&path).unwrap();
        assert_eq!(stamp.len, len);
        assert_eq!(stamp.hash, 0, "over-cap files use length-only stamps");
    }

    fn action(
        id: &str,
        action: &'static str,
        status: Result<(), String>,
    ) -> crate::loader::AppliedAction {
        crate::loader::AppliedAction {
            id: id.to_string(),
            action,
            status,
            verified: true,
        }
    }

    #[test]
    fn reload_outcome_classifies_actions() {
        // All-ok batch → Applied carrying the same actions.
        let ok_batch = vec![
            action("calc", "begin", Ok(())),
            action("llm", "update-config", Ok(())),
        ];
        match ReloadOutcome::classify(&ok_batch) {
            ReloadOutcome::Applied { actions } => assert_eq!(actions.len(), 2),
            other => panic!("all-ok batch must classify Applied, got {other:?}"),
        }

        // Any Err status ⇒ Failed carrying joined error text.
        let mixed = vec![
            action("calc", "begin", Ok(())),
            action("ghost", "rebuild-fiber", Err("no such plugin".to_string())),
            action("gone", "retire", Err("dispose failed".to_string())),
        ];
        match ReloadOutcome::classify(&mixed) {
            ReloadOutcome::Failed { error } => {
                assert!(error.contains("ghost"), "names failing id: {error}");
                assert!(error.contains("no such plugin"), "carries error: {error}");
                assert!(error.contains("gone"), "joins every error: {error}");
                assert!(error.contains("; "), "errors joined with '; '");
            }
            other => panic!("error batch must classify Failed, got {other:?}"),
        }

        // Empty batch ⇒ NoChange.
        assert!(matches!(
            ReloadOutcome::classify(&[]),
            ReloadOutcome::NoChange
        ));
    }

    #[test]
    fn reload_outcome_summary_strings() {
        assert_eq!(ReloadOutcome::NoChange.summary(), "no change");

        let ok_batch = vec![action("calc", "begin", Ok(()))];
        let applied = ReloadOutcome::classify(&ok_batch);
        let s = applied.summary();
        assert!(s.starts_with("applied 1:"), "applied summary: {s}");
        assert!(s.contains("begin:calc"), "lists action:id pairs: {s}");

        let bad = vec![action("x", "retire", Err("boom".to_string()))];
        let failed = ReloadOutcome::classify(&bad);
        let s = failed.summary();
        assert!(s.starts_with("failed:"), "failed summary: {s}");
        assert!(s.contains("boom"), "failed summary carries error: {s}");

        // Display delegates to summary.
        assert_eq!(format!("{applied}"), applied.summary());
    }
}
