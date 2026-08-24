//! Policy core of the tenant filesystem permission fence.
//!
//! This module holds ONLY the decision types and the L0/L1/L2 path checks.
//! No file-manipulation tool calls into it yet. Tool wiring arrives in a later
//! change. The full design lives in `docs/src/platform/tenant-fs-fence.md`.
//!
//! Layers, checked in order. A path passes only when every active layer passes:
//! - L0 mode: `ReadOnly` denies every write.
//! - L1 boundary: the resolved path stays inside `workspace_root`. `Full`
//!   mode waives this layer.
//! - L2 blocklist: a blocked name denies reads and writes in every mode.
//! - L3 write guards: not implemented here. They need the read-hash ledger of
//!   the future file tools, so they join at wiring time.

use std::borrow::Cow;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

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
