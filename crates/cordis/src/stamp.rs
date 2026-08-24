//! Content-hash change stamps for hot-reload decisions.
//!
//! [`FileStamp`] answers "did this file actually change?" without re-reading
//! it into config: a cheap length + content-hash pair. Watchers compare the
//! stamp captured at last load against the stamp of the file on disk, so
//! editor churn (touch, partial writes, mtime-only noise) that leaves bytes
//! identical no longer triggers a reload cycle.
//!
//! ## Planned: `ReloadOutcome` surfacing (design note — wiring next round)
//!
//! Today the reload pipeline computes rich results and throws them away:
//! `Loader::apply` / `Loader::reload_current` build
//! `Vec<AppliedAction { action, status, verified }>` (see `loader.rs`) but
//! the watcher boundary (`watcher.rs`, owned by a sibling this round)
//! discards them — an admin caller only learns "reload ran", never what it
//! did or why nothing happened.
//!
//! Plan:
//!
//! ```text
//! enum ReloadOutcome {
//!     NoChange,                       // FileStamp matched: skip entirely
//!     Applied  { actions: Vec<String> },   // human-readable AppliedAction lines
//!     Failed   { error: String },          // parse/apply failure, previous state intact
//! }
//! ```
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
}
