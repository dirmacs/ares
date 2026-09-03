//! Policy core of the tenant filesystem permission fence.
//!
//! This module holds the decision types, the L0/L1/L2 path checks, and the L3
//! write guards behind [`Fence`]. The full design lives in
//! `docs/src/platform/tenant-fs-fence.md`.
//!
//! Layers, checked in order. A path passes only when every active layer passes:
//! - L0 mode: `ReadOnly` denies every write.
//! - L1 boundary: the resolved path stays inside `workspace_root`. `Full`
//!   mode waives this layer.
//! - L2 blocklist: a blocked name denies reads and writes in every mode.
//! - L3 write guards: [`Fence`] records which paths a session observed
//!   through [`Fence::fence_read`] and gates [`Fence::fence_write`] on that
//!   record, unless the mode allows blind writes. Writes land through a
//!   temporary file and an atomic rename.
//!
//! `check_path`, `check_read`, and `check_write` stay pure path checks
//! (L0-L2). Only the [`Fence`] methods touch file contents.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Stable `FS_*` code for a filesystem error. Callers match on the code, so
/// agent-facing messages stay machine-readable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsError {
    /// One of the `FS_*` constants on this module's types.
    pub code: &'static str,
    pub message: String,
}

impl FsError {
    /// The path was never observed through [`Fence::fence_read`], so the
    /// session cannot prove it edits what it saw.
    pub const FS_NOT_OBSERVED: &'static str = "FS_NOT_OBSERVED";
    /// The file changed between the recorded observation and the write.
    pub const FS_VERSION_CONFLICT: &'static str = "FS_VERSION_CONFLICT";
    /// A guard demanded absence and the path already exists.
    pub const FS_EXISTS: &'static str = "FS_EXISTS";
    /// The fence layers refused the operation.
    pub const FS_FENCE_DENIED: &'static str = "FS_FENCE_DENIED";
    /// The underlying filesystem call failed.
    pub const FS_IO: &'static str = "FS_IO";

    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for FsError {}

/// Optimistic-concurrency contract for one write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteGuard {
    /// Skip read-before-edit. Only modes that allow blind writes accept it.
    Unconditional,
    /// Create the path; refuse when it already exists.
    CreateIfAbsent,
    /// Overwrite only when the file still matches the version captured at
    /// observation time.
    ReplaceIfVersion { version: u64 },
}

/// Cheap change fingerprint: `mtime_nanos ^ size`, saturating on clock
/// values outside the u64 range. Two writes that both move `mtime` and
/// `size` collide only in the same way a hash would; the guard treats any
/// difference as a concurrent modification.
fn version_fingerprint(metadata: &std::fs::Metadata) -> u64 {
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let size = metadata.len();
    mtime ^ size
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// One audit record from [`Fence::audit_log`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEntry {
    pub ts_millis: u64,
    /// Canonical path the operation targeted.
    pub path: PathBuf,
    /// Guard contract requested for the write (`None` for reads).
    pub guard_kind: Option<WriteGuard>,
    /// Stable `FS_*` code: `FS_OK` on success.
    pub outcome: &'static str,
}

/// Outcome code for successful fence operations in the audit ring.
pub const FS_OK: &str = "FS_OK";

/// Capacity of the bounded audit ring. Older entries drop first.
pub const AUDIT_CAPACITY: usize = 200;

const BLIND_WRITE_MODES: &[FenceMode] = &[FenceMode::Full];

#[derive(Debug, Default)]
struct FenceState {
    /// Canonical paths observed through `fence_read`, with their versions.
    observed: HashMap<PathBuf, u64>,
    /// Bounded audit ring; oldest entries leave first.
    audit: VecDeque<AuditEntry>,
}

impl FenceState {
    fn push_audit(&mut self, entry: AuditEntry) {
        if self.audit.len() >= AUDIT_CAPACITY {
            self.audit.pop_front();
        }
        self.audit.push_back(entry);
    }
}

/// Session-level L3 enforcement over a [`FencePolicy`].
///
/// The policy value stays shareable and pure; one `Fence` per session owns
/// the mutable observed-set and the audit ring behind a mutex.
#[derive(Debug)]
pub struct Fence {
    policy: FencePolicy,
    state: Mutex<FenceState>,
}

impl Fence {
    pub fn new(policy: FencePolicy) -> Self {
        Self {
            policy,
            state: Mutex::new(FenceState::default()),
        }
    }

    /// The immutable layer policy this fence enforces.
    pub fn policy(&self) -> &FencePolicy {
        &self.policy
    }

    /// Observe a path for a future guarded write. Runs L0-L2 first; a
    /// denied observation never enters the observed-set. Existing files
    /// record their version fingerprint; missing paths record version `0`.
    ///
    /// Returns `(metadata, version)`; metadata is `None` for missing paths.
    pub fn fence_read(&self, raw: &Path) -> Result<(Option<std::fs::Metadata>, u64), FsError> {
        let decision = check_path(&self.policy, raw, false);
        let Some(resolved) = resolve_allowed(&self.policy, raw, &decision)? else {
            self.record(raw, None, FsError::FS_FENCE_DENIED);
            return Err(FsError::new(
                FsError::FS_FENCE_DENIED,
                decision.denied_reason().to_string(),
            ));
        };

        let (metadata, version) = match std::fs::metadata(&resolved) {
            Ok(metadata) => {
                let version = version_fingerprint(&metadata);
                (Some(metadata), version)
            }
            // Reads of not-yet-existing paths are legal observations: they
            // register intent so CreateIfAbsent can later prove absence.
            Err(error) if error.kind() == ErrorKind::NotFound => (None, 0),
            Err(error) => return Err(FsError::new(FsError::FS_IO, error.to_string())),
        };

        let mut state = self.lock_state();
        state.observed.insert(resolved.clone(), version);
        state.push_audit(AuditEntry {
            ts_millis: now_unix_millis(),
            path: resolved,
            guard_kind: None,
            outcome: FS_OK,
        });
        Ok((metadata, version))
    }

    /// Write file contents under the L0-L2 layers plus the L3 guards.
    ///
    /// - Every write names a guard contract ([`WriteGuard`]).
    /// - In modes without blind-write allowance (`ReadOnly`,
    ///   `WorkspaceWrite`) the canonical path must have been observed
    ///   through [`Fence::fence_read`] first, or the write fails with
    ///   `FS_NOT_OBSERVED`. This covers every contract, including
    ///   [`WriteGuard::Unconditional`] and creating brand-new files:
    ///   only a mode that allows blind writes skips the ledger.
    /// - [`WriteGuard::CreateIfAbsent`] fails with `FS_EXISTS` when the
    ///   canonical path already exists.
    /// - [`WriteGuard::ReplaceIfVersion`] fails with `FS_VERSION_CONFLICT`
    ///   when the file is gone or its fingerprint differs from the version
    ///   captured at observation time.
    /// - Bytes land in a sibling temporary file renamed into place, so an
    ///   interrupted write leaves no torn file behind.
    pub fn fence_write(
        &self,
        raw: &Path,
        guard: WriteGuard,
        contents: &[u8],
    ) -> Result<(), FsError> {
        let blind_ok = BLIND_WRITE_MODES.contains(&self.policy.mode);

        // L0-L2 first, unchanged semantics.
        let decision = check_path(&self.policy, raw, true);
        let Some(resolved) = resolve_allowed(&self.policy, raw, &decision)? else {
            let reason = decision.denied_reason().to_string();
            self.record(raw, Some(guard), FsError::FS_FENCE_DENIED);
            return Err(FsError::new(FsError::FS_FENCE_DENIED, reason));
        };

        let current = match std::fs::metadata(&resolved) {
            Ok(metadata) => Some(version_fingerprint(&metadata)),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => {
                self.record(&resolved, Some(guard), FsError::FS_IO);
                return Err(FsError::new(FsError::FS_IO, error.to_string()));
            }
        };
        let exists = current.is_some();

        // L3 guard #1, read-before-edit: the session must hold an
        // observation of this exact canonical path unless the mode allows
        // blind writes. It runs first, so a never-read path always reports
        // FS_NOT_OBSERVED rather than a confusing contract mismatch.
        let holds_observation = self.lock_state().observed.contains_key(&resolved);
        if !holds_observation && !blind_ok {
            self.record(&resolved, Some(guard), FsError::FS_NOT_OBSERVED);
            return Err(FsError::new(
                FsError::FS_NOT_OBSERVED,
                format!(
                    "no recorded read for {}; call fence_read first",
                    resolved.display()
                ),
            ));
        }

        // Guard contract checks against the live filesystem view.
        match guard {
            WriteGuard::CreateIfAbsent if exists => {
                self.record(&resolved, Some(guard), FsError::FS_EXISTS);
                return Err(FsError::new(
                    FsError::FS_EXISTS,
                    format!(
                        "create refused, path already exists: {}",
                        resolved.display()
                    ),
                ));
            }
            WriteGuard::ReplaceIfVersion { version } if !exists || current != Some(version) => {
                self.record(&resolved, Some(guard), FsError::FS_VERSION_CONFLICT);
                return Err(FsError::new(
                    FsError::FS_VERSION_CONFLICT,
                    if exists {
                        format!(
                            "file changed since it was read (expected {version}, found {})",
                            current.unwrap_or_default()
                        )
                    } else {
                        "file disappeared since it was read".to_string()
                    },
                ));
            }
            _ => {}
        }

        atomic_write(&resolved, contents).map_err(|error| {
            self.record(&resolved, Some(guard), FsError::FS_IO);
            FsError::new(FsError::FS_IO, error.to_string())
        })?;

        // A successful write becomes the new observed version, so a follow-up
        // ReplaceIfVersion write chains off our own output instead of
        // conflicting with it.
        if let Ok(current) = std::fs::metadata(&resolved) {
            let version = version_fingerprint(&current);
            let mut state = self.lock_state();
            state.observed.insert(resolved.clone(), version);
        }

        self.record(&resolved, Some(guard), FS_OK);
        Ok(())
    }

    /// Snapshot of the bounded audit ring, oldest first.
    pub fn audit_log(&self) -> Vec<AuditEntry> {
        self.lock_state().audit.iter().cloned().collect()
    }
}

/// Sandbox mode for one session. Sessions switch modes at runtime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FenceMode {
    /// Deny every write. Reads still pass L1 and L2.
    ReadOnly,
    /// Default mode. Writes stay below the workspace root.
    #[default]
    WorkspaceWrite,
    /// Writes go anywhere that L2 allows. L1 steps aside in this mode.
    Full,
}

/// The fence layer that produced a denial. Callers put this name in errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FenceLayer {
    /// Session sandbox mode refused the operation.
    L0Mode,
    /// The path left the tenant workspace.
    L1Boundary,
    /// The path named a protected file.
    L2Blocklist,
    /// A write-time guard refused the operation (planned, see module docs).
    L3WriteGuard,
}

/// Verdict for one path check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FenceDecision {
    Allowed,
    Denied { layer: FenceLayer, reason: String },
}

impl FenceDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, FenceDecision::Allowed)
    }

    /// The human-readable cause behind a denial; empty for `Allowed`.
    pub fn denied_reason(&self) -> &str {
        match self {
            FenceDecision::Allowed => "",
            FenceDecision::Denied { reason, .. } => reason,
        }
    }

    fn deny(layer: FenceLayer, reason: impl Into<String>) -> Self {
        FenceDecision::Denied {
            layer,
            reason: reason.into(),
        }
    }
}

/// One tenant's fence configuration. Attach one instance per session.
#[derive(Clone, Debug)]
pub struct FencePolicy {
    pub mode: FenceMode,
    pub workspace_root: PathBuf,
    pub blocklist: Vec<String>,
}

impl FencePolicy {
    pub fn new(
        mode: FenceMode,
        workspace_root: impl Into<PathBuf>,
        blocklist: Vec<String>,
    ) -> Self {
        Self {
            mode,
            workspace_root: workspace_root.into(),
            blocklist,
        }
    }

    /// Judge a read. Runs L0, L1, and L2.
    pub fn check_read(&self, raw: &Path) -> FenceDecision {
        check_path(self, raw, false)
    }

    /// Judge a write. Runs L0, L1, and L2 today. L3 guards join at wiring
    /// time, once file tools carry the read-hash ledger.
    pub fn check_write(&self, raw: &Path) -> FenceDecision {
        check_path(self, raw, true)
    }
}

/// Core check. Judges `raw` for a read (`write == false`) or a write.
///
/// Layer order is fixed: L0, then L1, then L2. The first failing layer wins,
/// so the reported layer is deterministic for tests and for agent errors.
pub fn check_path(policy: &FencePolicy, raw: &Path, write: bool) -> FenceDecision {
    // L0: the session mode gate. Cheap, no filesystem access.
    if write && policy.mode == FenceMode::ReadOnly {
        return FenceDecision::deny(FenceLayer::L0Mode, "session is read-only");
    }

    // L1: the workspace boundary. Waived only in Full mode.
    let mut resolved: Option<PathBuf> = None;
    if policy.mode != FenceMode::Full {
        match resolve_against_root(policy, raw) {
            Ok(path) => resolved = Some(path),
            Err(reason) => return FenceDecision::deny(FenceLayer::L1Boundary, reason),
        }
    }

    // L2: the sensitive-name blocklist. Scans the raw spelling first, then
    // the canonical spelling, so a symlink cannot smuggle a protected name.
    if let Some(hit) = first_blocklisted(raw, &policy.blocklist) {
        return FenceDecision::deny(FenceLayer::L2Blocklist, format!("blocked name: {hit}"));
    }
    if let Some(resolved) = &resolved {
        if let Some(hit) = first_blocklisted(resolved, &policy.blocklist) {
            return FenceDecision::deny(FenceLayer::L2Blocklist, format!("blocked name: {hit}"));
        }
    }

    FenceDecision::Allowed
}

impl Fence {
    fn record(&self, path: &Path, guard_kind: Option<WriteGuard>, outcome: &'static str) {
        let mut state = self.lock_state();
        state.push_audit(AuditEntry {
            ts_millis: now_unix_millis(),
            path: path.to_path_buf(),
            guard_kind,
            outcome,
        });
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, FenceState> {
        // A panic while holding the fence mutex poisons it; recover to keep
        // the audit ring and observed-set usable for later calls.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Write `contents` through a unique temporary file next to `target`, then
/// rename over it. Best-effort `0600` on unix keeps tenant files private
/// even when the process umask is permissive.
fn atomic_write(target: &Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".ares-fence-tmp-{}-{nanos}", std::process::id()));
    let cleanup = |tmp: &Path| {
        let _ = std::fs::remove_file(tmp);
    };

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let file = std::fs::File::create(&tmp)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let mut writer = std::io::BufWriter::new(file);
        writer.write_all(contents)?;
        writer.flush()?;
        sync_parent_best_effort(dir);
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&tmp, contents)?;
    }

    match std::fs::rename(&tmp, target) {
        Ok(()) => Ok(()),
        Err(error) => {
            cleanup(&tmp);
            Err(error)
        }
    }
}

#[cfg(unix)]
fn sync_parent_best_effort(dir: &Path) {
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent_best_effort(_dir: &Path) {}

/// Canonical target path for an already-allowed decision. `None` means the
/// decision was a denial; an `Err` is the L1 reason for a denial that only
/// surfaces during resolution (workspace root unresolvable).
fn resolve_allowed(
    policy: &FencePolicy,
    raw: &Path,
    decision: &FenceDecision,
) -> Result<Option<PathBuf>, FsError> {
    if !decision.is_allowed() {
        return Ok(None);
    }
    // Full mode waives L1, so the raw spelling is the write target. Every
    // other mode resolves against the workspace root exactly like check_path
    // did during judgment; that resolution cannot fail here without the
    // decision having failed first.
    if policy.mode == FenceMode::Full {
        return Ok(Some(raw.to_path_buf()));
    }
    resolve_against_root(policy, raw)
        .map(Some)
        .map_err(|reason| FsError::new(FsError::FS_FENCE_DENIED, reason))
}

/// Anchor `raw` to the workspace root when relative, collapse `.` and `..`
/// lexically, then canonicalize the deepest existing ancestor and rejoin the
/// missing tail. Returns the path in canonical form.
fn resolve_against_root(policy: &FencePolicy, raw: &Path) -> Result<PathBuf, String> {
    let joined = if raw.is_absolute() {
        Cow::Borrowed(raw)
    } else {
        Cow::Owned(policy.workspace_root.join(raw))
    };

    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err("path climbs above the filesystem root".to_string());
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    let root = policy
        .workspace_root
        .canonicalize()
        .map_err(|error| format!("workspace root cannot be resolved: {error}"))?;

    let resolved =
        resolve_chain(&normalized).map_err(|error| format!("path cannot be resolved: {error}"))?;

    // Component-wise prefix test. Lexical neighbors such as `/root` and
    // `/root-evil` never satisfy `starts_with`.
    if !resolved.starts_with(&root) {
        return Err(format!("path leaves the workspace: {}", resolved.display()));
    }
    Ok(resolved)
}

/// Canonicalize the deepest existing ancestor of `candidate`, then append the
/// components that do not exist yet. Tail parts are fresh names, so they are
/// not symlinks and need no resolution.
fn resolve_chain(candidate: &Path) -> std::io::Result<PathBuf> {
    let mut tail: Vec<OsString> = Vec::new();
    let mut probe = candidate.to_path_buf();
    loop {
        match probe.canonicalize() {
            Ok(real) => {
                let mut resolved = real;
                for name in tail.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                match (probe.file_name(), probe.parent()) {
                    (Some(name), Some(parent)) => {
                        tail.push(name.to_os_string());
                        probe = parent.to_path_buf();
                    }
                    // No parent left and still not found: report as-is.
                    _ => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }
}

/// Return the first component that matches one blocklist pattern.
/// Matching is case-insensitive and supports `*` and `?` wildcards.
fn first_blocklisted(path: &Path, blocklist: &[String]) -> Option<String> {
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = name.to_string_lossy();
        for pattern in blocklist {
            if glob_match(
                pattern.to_lowercase().as_bytes(),
                name.to_lowercase().as_bytes(),
            ) {
                return Some(name.into_owned());
            }
        }
    }
    None
}

/// Hand-rolled fnmatch-style matcher. Supports the `*` and `?` wildcards,
/// which cover every pattern shape the fence documents.
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            glob_match(&pattern[1..], text) || (!text.is_empty() && glob_match(pattern, &text[1..]))
        }
        (Some(&p), Some(&t)) if p == b'?' || p == t => glob_match(&pattern[1..], &text[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Temporary directory with manual cleanup. The crate has no dev
    /// dependency on a temp-file helper, so this guard fills that role.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock runs")
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("ares-fence-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            TempDir(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn policy(mode: FenceMode, root: &Path) -> FencePolicy {
        FencePolicy::new(
            mode,
            root,
            vec![
                ".env".to_string(),
                ".env.*".to_string(),
                "*.pem".to_string(),
                "*.key".to_string(),
            ],
        )
    }

    #[test]
    fn read_only_blocks_write_and_allows_read() {
        let ws = TempDir::new("ro");
        let file = ws.path().join("notes.txt");
        std::fs::write(&file, b"hello").expect("seed file");

        let fence = policy(FenceMode::ReadOnly, ws.path());
        let write = fence.check_write(&file);
        let read = fence.check_read(&file);

        assert_eq!(
            write,
            FenceDecision::Denied {
                layer: FenceLayer::L0Mode,
                reason: write.expect_denied_reason(),
            }
        );
        assert!(read.is_allowed());
    }

    #[test]
    fn parent_traversal_escape_is_denied() {
        let ws = TempDir::new("traversal");
        let fence = policy(FenceMode::WorkspaceWrite, ws.path());

        let escape = ws.path().join("..").join("..").join("etc-passwd.txt");
        let decision = fence.check_write(&escape);

        assert_eq!(decision.layer(), Some(FenceLayer::L1Boundary));
    }

    #[test]
    fn missing_intermediate_directories_resolve() {
        let ws = TempDir::new("fresh");
        let fence = policy(FenceMode::WorkspaceWrite, ws.path());

        let fresh = ws.path().join("new-dir").join("report.txt");
        assert!(fence.check_write(&fresh).is_allowed());
        assert!(fence.check_read(&fresh).is_allowed());
    }

    #[test]
    #[cfg(unix)]
    fn symlink_escape_is_denied() {
        let ws = TempDir::new("sym-ws");
        let outside = TempDir::new("sym-out");
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, b"outside").expect("seed outside file");

        let link = ws.path().join("jump");
        std::os::unix::fs::symlink(&secret, &link).expect("create symlink");

        let fence = policy(FenceMode::WorkspaceWrite, ws.path());
        assert_eq!(
            fence.check_read(&link).layer(),
            Some(FenceLayer::L1Boundary)
        );
        assert_eq!(
            fence.check_write(&link).layer(),
            Some(FenceLayer::L1Boundary)
        );
    }

    #[test]
    fn blocklist_catches_env_at_any_depth() {
        let ws = TempDir::new("envdepth");
        let deep = ws.path().join("a").join("b");
        std::fs::create_dir_all(&deep).expect("create nested dirs");

        let fence = policy(FenceMode::WorkspaceWrite, ws.path());
        assert_eq!(
            fence.check_read(&deep.join(".env")).layer(),
            Some(FenceLayer::L2Blocklist)
        );
        assert_eq!(
            fence.check_write(&ws.path().join(".env.local")).layer(),
            Some(FenceLayer::L2Blocklist)
        );
    }

    #[test]
    fn blocklist_matches_wildcards_case_insensitive() {
        let ws = TempDir::new("pem");
        std::fs::write(ws.path().join("Server.PEM"), b"cert").expect("seed pem");

        let fence = policy(FenceMode::WorkspaceWrite, ws.path());
        assert_eq!(
            fence.check_read(&ws.path().join("Server.PEM")).layer(),
            Some(FenceLayer::L2Blocklist)
        );
        assert_eq!(
            fence.check_write(&ws.path().join("ca.key")).layer(),
            Some(FenceLayer::L2Blocklist)
        );
    }

    #[test]
    fn full_mode_waives_the_workspace_boundary() {
        let ws = TempDir::new("full-ws");
        let outside = TempDir::new("full-out");
        let fence = policy(FenceMode::Full, ws.path());

        let target = outside.path().join("notes.txt");
        assert!(fence.check_write(&target).is_allowed());
        assert!(fence.check_read(&target).is_allowed());
    }

    #[test]
    fn full_mode_still_enforces_the_blocklist() {
        let ws = TempDir::new("full-l2");
        let outside = TempDir::new("full-l2-out");
        let fence = policy(FenceMode::Full, ws.path());

        assert_eq!(
            fence
                .check_write(&outside.path().join("id_rsa_key.pem"))
                .layer(),
            Some(FenceLayer::L2Blocklist)
        );
        assert_eq!(
            fence.check_read(&ws.path().join(".env")).layer(),
            Some(FenceLayer::L2Blocklist)
        );
    }

    fn fence(mode: FenceMode, root: &Path) -> Fence {
        Fence::new(policy(mode, root))
    }

    /// Seed `contents` at `path` and record an observation through the fence,
    /// returning the version fingerprint for ReplaceIfVersion guards.
    fn observed_version(fence: &Fence, path: &Path, contents: &[u8]) -> u64 {
        std::fs::write(path, contents).expect("seed file");
        let (_, version) = fence.fence_read(path).expect("observe seeded file");
        version
    }

    #[test]
    fn write_requires_prior_read_in_l3() {
        let ws = TempDir::new("l3-unobserved");
        let target = ws.path().join("notes.txt");

        // WorkspaceWrite demands read-before-edit for every guard contract.
        let strict = fence(FenceMode::WorkspaceWrite, ws.path());
        let unconditional = strict.fence_write(&target, WriteGuard::Unconditional, b"no");
        let create = strict.fence_write(&target, WriteGuard::CreateIfAbsent, b"no");
        let replace =
            strict.fence_write(&target, WriteGuard::ReplaceIfVersion { version: 0 }, b"no");
        assert_eq!(unconditional.unwrap_err().code, FsError::FS_NOT_OBSERVED);
        assert_eq!(create.unwrap_err().code, FsError::FS_NOT_OBSERVED);
        assert_eq!(replace.unwrap_err().code, FsError::FS_NOT_OBSERVED);
        assert!(!target.exists(), "refused writes must not touch disk");

        // After an observation the guarded create goes through. The
        // missing path records version 0.
        let (_, version) = strict
            .fence_read(&target)
            .expect("observation of missing path");
        assert_eq!(version, 0);
        strict
            .fence_write(&target, WriteGuard::CreateIfAbsent, b"created")
            .expect("guarded create after observation");
        assert_eq!(std::fs::read(&target).unwrap(), b"created");

        // Full mode allows blind writes without any observation.
        let blind = fence(FenceMode::Full, ws.path());
        blind
            .fence_write(&target, WriteGuard::Unconditional, b"blind")
            .expect("blind write allowed in Full mode");
        assert_eq!(std::fs::read(&target).unwrap(), b"blind");

        // ReadOnly still denies at L0 before any guard runs.
        let frozen = fence(FenceMode::ReadOnly, ws.path());
        let denied = frozen.fence_write(&target, WriteGuard::Unconditional, b"x");
        assert_eq!(denied.unwrap_err().code, FsError::FS_FENCE_DENIED);
        assert_eq!(
            frozen.policy().mode,
            FenceMode::ReadOnly,
            "policy stays immutable"
        );
    }

    #[test]
    fn replace_if_version_conflicts_on_concurrent_change() {
        let ws = TempDir::new("l3-conflict");
        let target = ws.path().join("notes.txt");
        let f = fence(FenceMode::WorkspaceWrite, ws.path());

        let version = observed_version(&f, &target, b"first draft");

        // Same version passes and lands atomically.
        f.fence_write(
            &target,
            WriteGuard::ReplaceIfVersion { version },
            b"second draft",
        )
        .expect("matching version writes");

        // A concurrent writer moves mtime+size after our observation.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&target, b"a concurrent writer got here first").expect("concurrent edit");

        let conflict = f.fence_write(
            &target,
            WriteGuard::ReplaceIfVersion { version },
            b"stale overwrite",
        );
        let error = conflict.expect_err("stale version must conflict");
        assert_eq!(error.code, FsError::FS_VERSION_CONFLICT);
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"a concurrent writer got here first",
            "conflicted write must not clobber the concurrent change"
        );

        // The successful chained write refreshed the observation, so a second
        // edit with a freshly captured version applies cleanly.
        let (_, fresh) = f.fence_read(&target).expect("re-read");
        f.fence_write(
            &target,
            WriteGuard::ReplaceIfVersion { version: fresh },
            b"third draft",
        )
        .expect("fresh version writes");
        assert_eq!(std::fs::read(&target).unwrap(), b"third draft");
    }

    #[test]
    fn replace_if_version_refuses_disappeared_file() {
        let ws = TempDir::new("l3-gone");
        let target = ws.path().join("notes.txt");
        let f = fence(FenceMode::WorkspaceWrite, ws.path());

        let version = observed_version(&f, &target, b"do not lose me");
        std::fs::remove_file(&target).expect("delete behind the fence");

        let error = f
            .fence_write(
                &target,
                WriteGuard::ReplaceIfVersion { version },
                b"resurrect",
            )
            .expect_err("missing file must refuse replacement");
        assert_eq!(error.code, FsError::FS_VERSION_CONFLICT);
    }

    #[test]
    fn create_if_absent_refuses_overwrite() {
        let ws = TempDir::new("l3-create");
        let fresh = ws.path().join("fresh.txt");
        let taken = ws.path().join("taken.txt");
        let f = fence(FenceMode::WorkspaceWrite, ws.path());

        f.fence_read(&fresh).expect("observe absent fresh path");
        f.fence_write(&fresh, WriteGuard::CreateIfAbsent, b"v1")
            .expect("create on absent path");
        assert_eq!(std::fs::read(&fresh).unwrap(), b"v1");

        // Overwrite attempt fails with FS_EXISTS even though we observed it.
        let error = f
            .fence_write(&fresh, WriteGuard::CreateIfAbsent, b"clobber")
            .expect_err("second create must refuse");
        assert_eq!(error.code, FsError::FS_EXISTS);
        assert_eq!(std::fs::read(&fresh).unwrap(), b"v1", "content preserved");

        // Unobserved creation still requires read-before-edit in this mode.
        let error = f
            .fence_write(&taken, WriteGuard::CreateIfAbsent, b"no")
            .expect_err("unobserved create needs an observation");
        assert_eq!(error.code, FsError::FS_NOT_OBSERVED);
        assert!(!taken.exists());
    }

    #[test]
    fn audit_ring_trims_at_capacity() {
        let ws = TempDir::new("l3-audit");
        let target = ws.path().join("loop.txt");
        let f = fence(FenceMode::Full, ws.path());

        for round in 0..(AUDIT_CAPACITY + 25) {
            f.fence_write(
                &target,
                WriteGuard::Unconditional,
                format!("round {round}").as_bytes(),
            )
            .expect("blind writes run in Full mode");
        }

        let log = f.audit_log();
        assert_eq!(log.len(), AUDIT_CAPACITY);
        // The oldest 25 entries left; the ring holds the last 200 writes.
        assert!(log.iter().all(|entry| entry.outcome == FS_OK));
        assert!(log
            .iter()
            .all(|entry| entry.guard_kind == Some(WriteGuard::Unconditional)));
        assert!(
            log.first().expect("ring nonempty").ts_millis > 0,
            "entries carry timestamps"
        );
    }

    #[test]
    fn l0_l2_paths_unchanged() {
        let ws = TempDir::new("l0l2-regression");

        // L0: pure path check still denies ReadOnly writes...
        let ro_policy = policy(FenceMode::ReadOnly, ws.path());
        let victim = ws.path().join("victim.txt");
        std::fs::write(&victim, b"keep").expect("seed victim");
        assert!(!ro_policy.check_write(&victim).is_allowed());
        assert!(ro_policy.check_read(&victim).is_allowed());
        // ...and the Fence path refuses before any L3 logic or IO.
        let frozen = fence(FenceMode::ReadOnly, ws.path());
        let denied = frozen.fence_write(&victim, WriteGuard::Unconditional, b"x");
        assert_eq!(denied.unwrap_err().code, FsError::FS_FENCE_DENIED);
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep", "L0 protects");

        // L1: traversal escape still denied through the Fence.
        let escaper = fence(FenceMode::WorkspaceWrite, ws.path());
        let escape = ws.path().join("..").join("escape.txt");
        let error = escaper
            .fence_write(&escape, WriteGuard::Unconditional, b"x")
            .expect_err("escape refused");
        assert_eq!(error.code, FsError::FS_FENCE_DENIED);
        assert!(!escape.exists());

        // L2: blocklist still wins over everything, including Full mode.
        let full = fence(FenceMode::Full, ws.path());
        let pem = ws.path().join("secret.key");
        let error = full
            .fence_write(&pem, WriteGuard::Unconditional, b"k")
            .expect_err("blocklist hit refused");
        assert_eq!(error.code, FsError::FS_FENCE_DENIED);
        assert!(full.fence_read(&ws.path().join(".env")).is_err());
        assert!(!pem.exists());
    }

    #[test]
    fn atomic_write_leaves_no_temp_files_and_sets_private_mode() {
        let ws = TempDir::new("l3-atomic");
        let target = ws.path().join("private.txt");
        let f = fence(FenceMode::Full, ws.path());

        f.fence_write(&target, WriteGuard::Unconditional, b"payload")
            .expect("write lands");
        let leftovers: Vec<_> = std::fs::read_dir(ws.path())
            .expect("list workspace")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            leftovers,
            vec!["private.txt".to_string()],
            "tmp renamed away"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "best-effort chmod 0600 applied");
        }
    }

    #[test]
    fn audit_records_deny_and_success_outcomes() {
        let ws = TempDir::new("l3-audit-mixed");
        let good = ws.path().join("good.txt");
        let bad = ws.path().join(".env");
        let f = fence(FenceMode::WorkspaceWrite, ws.path());

        // A blocklisted read is refused before any observation.
        let error = f.fence_read(&bad).expect_err("blocklisted read refused");
        assert_eq!(error.code, FsError::FS_FENCE_DENIED);

        // An unobserved guarded create lands in the ring as FS_NOT_OBSERVED.
        let error = f
            .fence_write(&good, WriteGuard::CreateIfAbsent, b"ok")
            .expect_err("create without observation refused");
        assert_eq!(error.code, FsError::FS_NOT_OBSERVED);

        // Observe, then succeed.
        f.fence_read(&good).expect("observe absent path");
        f.fence_write(&good, WriteGuard::CreateIfAbsent, b"ok")
            .expect("guarded create after observation");

        let log = f.audit_log();
        let outcomes: Vec<&str> = log.iter().map(|entry| entry.outcome).collect();
        assert_eq!(
            outcomes,
            vec![
                FsError::FS_FENCE_DENIED,
                FsError::FS_NOT_OBSERVED,
                FS_OK,
                FS_OK
            ]
        );
        // Reads carry no guard contract; writes do.
        assert_eq!(log[0].guard_kind, None);
        assert_eq!(log[1].guard_kind, Some(WriteGuard::CreateIfAbsent));
    }

    impl FenceDecision {
        fn expect_denied_reason(&self) -> String {
            match self {
                FenceDecision::Denied { reason, .. } => reason.clone(),
                FenceDecision::Allowed => panic!("expected a denial"),
            }
        }

        fn layer(&self) -> Option<FenceLayer> {
            match self {
                FenceDecision::Denied { layer, .. } => Some(*layer),
                FenceDecision::Allowed => None,
            }
        }
    }
}
