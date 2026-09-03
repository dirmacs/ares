//! Agent checkpoint/crash recovery system.
//!
//! Serializes agent state to disk before each step. On restart,
//! restores from the latest checkpoint and resumes execution.
//!
//! Inspired by Octopoda-OS crash recovery patterns.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

/// Agent state captured at a point in time (alias for [`Checkpoint`]).
pub type AgentCheckpoint = Checkpoint;

/// A checkpoint captures agent state at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Checkpoint {
    /// Unique checkpoint ID
    pub id: String,
    /// Agent name/type
    pub agent_name: String,
    /// Session ID
    pub session_id: String,
    /// Step number (0-indexed)
    pub step: usize,
    /// Conversation messages so far
    pub messages: Vec<CheckpointMessage>,
    /// Tool calls made and their results
    pub tool_calls: Vec<ToolCallRecord>,
    /// Partial results accumulated
    pub partial_results: Vec<String>,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
    /// Optional TTL in seconds from `timestamp`.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
    /// Status of this checkpoint
    pub status: CheckpointStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CheckpointMessage {
    pub role: String, // "user" | "assistant" | "system"
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub arguments: String,
    pub result: Option<String>,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CheckpointStatus {
    /// Agent is actively running
    InProgress,
    /// Agent completed successfully
    Completed,
    /// Agent failed/crashed
    Failed(String),
    /// Agent was halted (e.g., by loop detector)
    Halted(String),
}

/// On-disk / index metadata for a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointMetadata {
    pub agent_id: String,
    pub run_id: String,
    pub step: usize,
    pub timestamp: u64,
    /// Optional TTL in seconds from `timestamp`.
    pub ttl_secs: Option<u64>,
}

/// Errors from checkpoint serialization and lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointError {
    NotFound(String),
    Corrupt(String),
    IoError(String),
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(msg) => write!(f, "checkpoint not found: {msg}"),
            Self::Corrupt(msg) => write!(f, "checkpoint corrupt: {msg}"),
            Self::IoError(msg) => write!(f, "checkpoint io error: {msg}"),
        }
    }
}

impl std::error::Error for CheckpointError {}

impl fmt::Display for Checkpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Checkpoint {{ id: {}, agent: {}, session: {}, step: {}, status: {:?} }}",
            self.id, self.agent_name, self.session_id, self.step, self.status
        )
    }
}

impl fmt::Display for CheckpointMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CheckpointMetadata {{ agent_id: {}, run_id: {}, step: {}, ts: {} }}",
            self.agent_id, self.run_id, self.step, self.timestamp
        )
    }
}

impl From<&Checkpoint> for CheckpointMetadata {
    fn from(cp: &Checkpoint) -> Self {
        Self {
            agent_id: cp.agent_name.clone(),
            run_id: cp.session_id.clone(),
            step: cp.step,
            timestamp: cp.timestamp,
            ttl_secs: cp.ttl_secs,
        }
    }
}

/// Stable storage key for an agent run (`agent_id/run_id`).
pub fn checkpoint_key(agent_id: &str, run_id: &str) -> String {
    format!("{agent_id}/{run_id}")
}

/// Build a new in-memory checkpoint for the given agent run and step.
#[allow(clippy::too_many_arguments)]
pub fn create_checkpoint(
    agent_id: &str,
    run_id: &str,
    step: usize,
    messages: Vec<CheckpointMessage>,
    tool_calls: Vec<ToolCallRecord>,
    partial_results: Vec<String>,
    timestamp: u64,
    status: CheckpointStatus,
) -> AgentCheckpoint {
    create_checkpoint_with_ttl(
        agent_id,
        run_id,
        step,
        messages,
        tool_calls,
        partial_results,
        timestamp,
        None,
        status,
    )
}

/// Like [`create_checkpoint`] but attaches an optional TTL.
#[allow(clippy::too_many_arguments)]
pub fn create_checkpoint_with_ttl(
    agent_id: &str,
    run_id: &str,
    step: usize,
    messages: Vec<CheckpointMessage>,
    tool_calls: Vec<ToolCallRecord>,
    partial_results: Vec<String>,
    timestamp: u64,
    ttl_secs: Option<u64>,
    status: CheckpointStatus,
) -> AgentCheckpoint {
    let key = checkpoint_key(agent_id, run_id);
    AgentCheckpoint {
        id: format!("{key}:step{step}"),
        agent_name: agent_id.to_string(),
        session_id: run_id.to_string(),
        step,
        messages,
        tool_calls,
        partial_results,
        timestamp,
        ttl_secs,
        status,
    }
}

/// Serialize checkpoint state to JSON bytes.
pub fn serialize_state(checkpoint: &AgentCheckpoint) -> Result<Vec<u8>, CheckpointError> {
    serde_json::to_vec(checkpoint).map_err(|e| CheckpointError::IoError(e.to_string()))
}

/// Restore checkpoint state from serialized JSON bytes.
pub fn restore_checkpoint(bytes: &[u8]) -> Result<AgentCheckpoint, CheckpointError> {
    if bytes.is_empty() {
        return Err(CheckpointError::Corrupt("empty payload".into()));
    }
    serde_json::from_slice(bytes).map_err(|e| CheckpointError::Corrupt(e.to_string()))
}

/// Returns true when `now` is past `metadata.timestamp + ttl_secs`.
pub fn is_expired(metadata: &CheckpointMetadata, now: u64) -> bool {
    metadata
        .ttl_secs
        .is_some_and(|ttl| now > metadata.timestamp.saturating_add(ttl))
}

/// Return metadata entries that have expired relative to `now`.
pub fn expired_metadata<'a>(
    entries: impl IntoIterator<Item = &'a CheckpointMetadata>,
    now: u64,
) -> Vec<&'a CheckpointMetadata> {
    entries.into_iter().filter(|m| is_expired(m, now)).collect()
}

fn checkpoint_step_filename(run_id: &str, step: usize) -> String {
    format!("{run_id}_{step}.json")
}

/// Manages checkpoints for agent crash recovery.
pub struct CheckpointManager {
    /// Directory to store checkpoint files
    checkpoint_dir: PathBuf,
}

impl CheckpointManager {
    /// Create a new checkpoint manager.
    pub fn new(checkpoint_dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(checkpoint_dir)?;
        Ok(Self {
            checkpoint_dir: checkpoint_dir.to_path_buf(),
        })
    }

    /// Create a default checkpoint manager (~/.ares/checkpoints/).
    pub fn default_dir() -> std::io::Result<Self> {
        let dir = dirs_or_default().join("checkpoints");
        Self::new(&dir)
    }

    /// Save a checkpoint to disk.
    pub fn save(&self, checkpoint: &Checkpoint) -> std::io::Result<()> {
        let filename = checkpoint_step_filename(&checkpoint.session_id, checkpoint.step);
        let path = self.checkpoint_dir.join(&filename);
        let bytes =
            serialize_state(checkpoint).map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(&path, bytes)?;

        // Also update the "latest" symlink/pointer
        let latest_path = self
            .checkpoint_dir
            .join(format!("{}_latest.json", checkpoint.session_id));
        std::fs::write(&latest_path, &filename)?;

        Ok(())
    }

    /// Load the latest checkpoint for a session.
    pub fn load_latest(&self, session_id: &str) -> std::io::Result<Option<Checkpoint>> {
        let latest_path = self
            .checkpoint_dir
            .join(format!("{}_latest.json", session_id));
        if !latest_path.exists() {
            return Ok(None);
        }

        let filename = std::fs::read_to_string(&latest_path)?;
        let checkpoint_path = self.checkpoint_dir.join(filename.trim());
        if !checkpoint_path.exists() {
            return Ok(None);
        }

        let bytes = std::fs::read(&checkpoint_path)?;
        let checkpoint = restore_checkpoint(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        Ok(Some(checkpoint))
    }

    /// List all checkpoints for a session, ordered by step.
    pub fn list_checkpoints(&self, session_id: &str) -> std::io::Result<Vec<Checkpoint>> {
        let mut checkpoints = Vec::new();
        let prefix = format!("{}_", session_id);

        for entry in std::fs::read_dir(&self.checkpoint_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) && name.ends_with(".json") && !name.contains("latest") {
                let bytes = std::fs::read(entry.path())?;
                if let Ok(cp) = restore_checkpoint(&bytes) {
                    checkpoints.push(cp);
                }
            }
        }

        checkpoints.sort_by_key(|c| c.step);
        Ok(checkpoints)
    }

    /// Clean up old checkpoints for a completed session.
    pub fn cleanup(&self, session_id: &str) -> std::io::Result<usize> {
        let mut removed = 0;
        let prefix = format!("{}_", session_id);

        for entry in std::fs::read_dir(&self.checkpoint_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) {
                std::fs::remove_file(entry.path())?;
                removed += 1;
            }
        }

        Ok(removed)
    }

    /// Remove checkpoint files whose metadata has expired relative to `now`.
    pub fn cleanup_expired(&self, now: u64) -> std::io::Result<usize> {
        let mut removed = 0;
        for entry in std::fs::read_dir(&self.checkpoint_dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".json") || name.contains("latest") {
                continue;
            }
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let Ok(cp) = restore_checkpoint(&bytes) else {
                continue;
            };
            if is_expired(&CheckpointMetadata::from(&cp), now) {
                std::fs::remove_file(path)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Check if a session has a recoverable checkpoint.
    pub fn has_checkpoint(&self, session_id: &str) -> bool {
        let latest_path = self
            .checkpoint_dir
            .join(format!("{}_latest.json", session_id));
        latest_path.exists()
    }
}

fn dirs_or_default() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("ares")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn sample_messages() -> Vec<CheckpointMessage> {
        vec![
            CheckpointMessage {
                role: "user".into(),
                content: "Hello".into(),
            },
            CheckpointMessage {
                role: "assistant".into(),
                content: "Hi there".into(),
            },
        ]
    }

    fn sample_tool_calls() -> Vec<ToolCallRecord> {
        vec![ToolCallRecord {
            tool_name: "search".into(),
            arguments: "query".into(),
            result: Some("found it".into()),
            success: true,
        }]
    }

    fn sample_checkpoint(session: &str, step: usize) -> Checkpoint {
        create_checkpoint(
            "test-agent",
            session,
            step,
            sample_messages(),
            sample_tool_calls(),
            vec!["partial output".into()],
            now_secs(),
            CheckpointStatus::InProgress,
        )
    }

    #[test]
    fn checkpoint_key_uses_agent_id_run_id_format() {
        assert_eq!(checkpoint_key("agent-a", "run-42"), "agent-a/run-42");
    }

    #[test]
    fn checkpoint_key_unique_per_pair() {
        assert_ne!(checkpoint_key("a1", "r1"), checkpoint_key("a1", "r2"));
        assert_ne!(checkpoint_key("a1", "r1"), checkpoint_key("a2", "r1"));
    }

    #[test]
    fn create_checkpoint_sets_ids_and_fields() {
        let cp = create_checkpoint(
            "researcher",
            "run-9",
            2,
            sample_messages(),
            sample_tool_calls(),
            vec!["out".into()],
            1_700_000_000,
            CheckpointStatus::InProgress,
        );
        assert_eq!(cp.agent_name, "researcher");
        assert_eq!(cp.session_id, "run-9");
        assert_eq!(cp.step, 2);
        assert!(cp.id.contains("researcher/run-9"));
    }

    #[test]
    fn serialize_state_and_restore_checkpoint_are_symmetric() {
        let cp = sample_checkpoint("sym", 1);
        let bytes = serialize_state(&cp).unwrap();
        assert_eq!(restore_checkpoint(&bytes).unwrap(), cp);
    }

    #[test]
    fn restore_checkpoint_rejects_empty_bytes() {
        assert!(matches!(
            restore_checkpoint(&[]),
            Err(CheckpointError::Corrupt(_))
        ));
    }

    #[test]
    fn restore_checkpoint_rejects_invalid_json() {
        assert!(matches!(
            restore_checkpoint(b"{not json"),
            Err(CheckpointError::Corrupt(_))
        ));
    }

    #[test]
    fn agent_checkpoint_serde_json_roundtrip() {
        let cp = sample_checkpoint("serde-cp", 0);
        let json = serde_json::to_string(&cp).unwrap();
        let back: AgentCheckpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cp);
    }

    #[test]
    fn checkpoint_metadata_serde_json_roundtrip() {
        let meta = CheckpointMetadata {
            agent_id: "agent-x".into(),
            run_id: "run-y".into(),
            step: 5,
            timestamp: 100,
            ttl_secs: Some(3600),
        };
        let back: CheckpointMetadata =
            serde_json::from_str(&serde_json::to_string(&meta).unwrap()).unwrap();
        assert_eq!(back, meta);
    }

    #[test]
    fn checkpoint_metadata_from_checkpoint() {
        let cp = sample_checkpoint("run-meta", 3);
        let meta = CheckpointMetadata::from(&cp);
        assert_eq!(meta.agent_id, "test-agent");
        assert_eq!(meta.run_id, "run-meta");
        assert_eq!(meta.step, 3);
        assert!(meta.ttl_secs.is_none());
    }

    #[test]
    fn is_expired_false_without_ttl() {
        let meta = CheckpointMetadata {
            agent_id: "a".into(),
            run_id: "r".into(),
            step: 0,
            timestamp: 100,
            ttl_secs: None,
        };
        assert!(!is_expired(&meta, 999_999));
    }

    #[test]
    fn is_expired_true_past_ttl() {
        let meta = CheckpointMetadata {
            agent_id: "a".into(),
            run_id: "r".into(),
            step: 0,
            timestamp: 100,
            ttl_secs: Some(60),
        };
        assert!(!is_expired(&meta, 160));
        assert!(is_expired(&meta, 161));
    }

    #[test]
    fn expired_metadata_filters_only_stale() {
        let fresh = CheckpointMetadata {
            agent_id: "a".into(),
            run_id: "fresh".into(),
            step: 0,
            timestamp: 180,
            ttl_secs: Some(60),
        };
        let stale = CheckpointMetadata {
            agent_id: "a".into(),
            run_id: "stale".into(),
            step: 0,
            timestamp: 100,
            ttl_secs: Some(60),
        };
        let expired = expired_metadata([&fresh, &stale], 200);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].run_id, "stale");
    }

    #[test]
    fn checkpoint_error_not_found_display() {
        let err = CheckpointError::NotFound("sess-1".into());
        assert_eq!(err.to_string(), "checkpoint not found: sess-1");
    }

    #[test]
    fn checkpoint_error_corrupt_display() {
        let err = CheckpointError::Corrupt("bad json".into());
        assert_eq!(err.to_string(), "checkpoint corrupt: bad json");
    }

    #[test]
    fn checkpoint_error_io_error_display() {
        let err = CheckpointError::IoError("disk full".into());
        assert_eq!(err.to_string(), "checkpoint io error: disk full");
    }

    #[test]
    fn checkpoint_error_debug_contains_variant() {
        let dbg = format!("{:?}", CheckpointError::NotFound("x".into()));
        assert!(dbg.contains("NotFound"));
    }

    #[test]
    fn checkpoint_clone_equals_original() {
        let cp = sample_checkpoint("clone", 0);
        assert_eq!(cp.clone(), cp);
    }

    #[test]
    fn checkpoint_metadata_clone_equals_original() {
        let meta = CheckpointMetadata::from(&sample_checkpoint("clone-meta", 1));
        assert_eq!(meta.clone(), meta);
    }

    #[test]
    fn checkpoint_display_includes_ids() {
        let s = sample_checkpoint("disp", 4).to_string();
        assert!(s.contains("disp"));
        assert!(s.contains("test-agent"));
    }

    #[test]
    fn checkpoint_metadata_display_includes_run_id() {
        let s = CheckpointMetadata::from(&sample_checkpoint("disp-meta", 0)).to_string();
        assert!(s.contains("disp-meta"));
    }

    #[test]
    fn checkpoint_debug_impl_available() {
        let dbg = format!("{:?}", sample_checkpoint("dbg", 0));
        assert!(dbg.contains("Checkpoint"));
    }

    #[test]
    fn test_save_and_load() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        let cp = sample_checkpoint("sess1", 0);
        mgr.save(&cp).unwrap();
        let loaded = mgr.load_latest("sess1").unwrap().unwrap();
        assert_eq!(loaded.session_id, "sess1");
        assert_eq!(loaded.step, 0);
    }

    #[test]
    fn test_load_nonexistent() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        assert!(mgr.load_latest("nonexistent").unwrap().is_none());
    }

    #[test]
    fn test_multiple_steps() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        mgr.save(&sample_checkpoint("sess1", 0)).unwrap();
        mgr.save(&sample_checkpoint("sess1", 1)).unwrap();
        mgr.save(&sample_checkpoint("sess1", 2)).unwrap();
        assert_eq!(mgr.load_latest("sess1").unwrap().unwrap().step, 2);
        assert_eq!(mgr.list_checkpoints("sess1").unwrap().len(), 3);
    }

    #[test]
    fn test_cleanup() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        mgr.save(&sample_checkpoint("sess1", 0)).unwrap();
        mgr.save(&sample_checkpoint("sess1", 1)).unwrap();
        assert!(mgr.cleanup("sess1").unwrap() >= 2);
        assert!(!mgr.has_checkpoint("sess1"));
    }

    #[test]
    fn cleanup_expired_removes_stale_files_only() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        let now = now_secs();
        let stale = create_checkpoint_with_ttl(
            "test-agent",
            "expired-run",
            0,
            sample_messages(),
            sample_tool_calls(),
            vec![],
            now.saturating_sub(10_000),
            Some(60),
            CheckpointStatus::InProgress,
        );
        mgr.save(&stale).unwrap();
        mgr.save(&sample_checkpoint("fresh-run", 0)).unwrap();
        assert_eq!(mgr.cleanup_expired(now).unwrap(), 1);
        assert!(mgr.load_latest("expired-run").unwrap().is_none());
        assert!(mgr.load_latest("fresh-run").unwrap().is_some());
    }

    #[test]
    fn test_separate_sessions() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        mgr.save(&sample_checkpoint("sess1", 0)).unwrap();
        mgr.save(&sample_checkpoint("sess2", 0)).unwrap();
        mgr.cleanup("sess1").unwrap();
        assert!(!mgr.has_checkpoint("sess1"));
        assert!(mgr.has_checkpoint("sess2"));
    }

    #[test]
    fn test_checkpoint_status_serialization() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        let mut cp = sample_checkpoint("sess1", 0);
        cp.status = CheckpointStatus::Failed("OOM".into());
        mgr.save(&cp).unwrap();
        assert_eq!(
            mgr.load_latest("sess1").unwrap().unwrap().status,
            CheckpointStatus::Failed("OOM".into())
        );
    }

    #[test]
    fn test_restore_preserves_all_checkpoint_fields() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        let mut cp = sample_checkpoint("sess-restore", 3);
        cp.id = "cp-full".into();
        cp.agent_name = "researcher".into();
        cp.partial_results = vec!["chunk-a".into(), "chunk-b".into()];
        cp.status = CheckpointStatus::Completed;
        mgr.save(&cp).unwrap();
        let loaded = mgr.load_latest("sess-restore").unwrap().unwrap();
        assert_eq!(loaded.id, cp.id);
        assert_eq!(loaded.messages.len(), cp.messages.len());
        assert_eq!(loaded.status, cp.status);
    }

    #[test]
    fn test_checkpoint_status_completed_and_halted_round_trip() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        let mut completed = sample_checkpoint("sess-status", 0);
        completed.status = CheckpointStatus::Completed;
        mgr.save(&completed).unwrap();
        assert_eq!(
            mgr.load_latest("sess-status").unwrap().unwrap().status,
            CheckpointStatus::Completed
        );
        let mut halted = sample_checkpoint("sess-status", 1);
        halted.status = CheckpointStatus::Halted("loop detected".into());
        mgr.save(&halted).unwrap();
        assert_eq!(
            mgr.load_latest("sess-status").unwrap().unwrap().status,
            CheckpointStatus::Halted("loop detected".into())
        );
    }

    #[test]
    fn new_fails_when_checkpoint_path_is_a_file() {
        let dir = temp_dir();
        let file_path = dir.path().join("not_a_dir");
        std::fs::write(&file_path, "blocking file").unwrap();
        assert!(CheckpointManager::new(&file_path).is_err());
    }

    #[test]
    fn load_latest_invalid_json_returns_invalid_data() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        let session = "bad-json";
        std::fs::write(
            dir.path().join(format!("{session}_0.json")),
            "not valid json",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(format!("{session}_latest.json")),
            format!("{session}_0.json"),
        )
        .unwrap();
        assert_eq!(
            mgr.load_latest(session).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn load_latest_stale_pointer_returns_none() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("stale-pointer_latest.json"),
            "missing_checkpoint_file.json",
        )
        .unwrap();
        assert!(mgr.load_latest("stale-pointer").unwrap().is_none());
    }

    #[test]
    fn list_checkpoints_skips_corrupt_entries() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        let session = "sess-skip";
        mgr.save(&sample_checkpoint(session, 0)).unwrap();
        mgr.save(&sample_checkpoint(session, 1)).unwrap();
        std::fs::write(dir.path().join(format!("{session}_corrupt.json")), "{bad").unwrap();
        let listed = mgr.list_checkpoints(session).unwrap();
        assert_eq!(listed.len(), 2);
    }

    #[test]
    fn has_checkpoint_false_without_latest_pointer() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        mgr.save(&sample_checkpoint("sess-has", 0)).unwrap();
        std::fs::remove_file(dir.path().join("sess-has_latest.json")).unwrap();
        assert!(!mgr.has_checkpoint("sess-has"));
    }

    #[test]
    fn default_dir_initializes_manager() {
        let mgr = CheckpointManager::default_dir().expect("default_dir should succeed");
        assert!(mgr
            .checkpoint_dir
            .to_string_lossy()
            .ends_with("checkpoints"));
    }

    #[test]
    fn manager_save_restore_uses_serialize_state_symmetry() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        let cp = sample_checkpoint("roundtrip", 7);
        mgr.save(&cp).unwrap();
        let bytes = std::fs::read(dir.path().join("roundtrip_7.json")).unwrap();
        assert_eq!(restore_checkpoint(&bytes).unwrap(), cp);
    }
}
