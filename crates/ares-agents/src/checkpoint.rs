//! Agent checkpoint/crash recovery system.
//!
//! Serializes agent state to disk before each step. On restart,
//! restores from the latest checkpoint and resumes execution.
//!
//! Inspired by Octopoda-OS crash recovery patterns.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A checkpoint captures agent state at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        let filename = format!("{}_{}.json", checkpoint.session_id, checkpoint.step);
        let path = self.checkpoint_dir.join(&filename);
        let json = serde_json::to_string_pretty(checkpoint)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(&path, json)?;

        // Also update the "latest" symlink/pointer
        let latest_path = self.checkpoint_dir.join(format!("{}_latest.json", checkpoint.session_id));
        std::fs::write(&latest_path, &filename)?;

        Ok(())
    }

    /// Load the latest checkpoint for a session.
    pub fn load_latest(&self, session_id: &str) -> std::io::Result<Option<Checkpoint>> {
        let latest_path = self.checkpoint_dir.join(format!("{}_latest.json", session_id));
        if !latest_path.exists() {
            return Ok(None);
        }

        let filename = std::fs::read_to_string(&latest_path)?;
        let checkpoint_path = self.checkpoint_dir.join(filename.trim());
        if !checkpoint_path.exists() {
            return Ok(None);
        }

        let json = std::fs::read_to_string(&checkpoint_path)?;
        let checkpoint: Checkpoint = serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
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
                let json = std::fs::read_to_string(entry.path())?;
                if let Ok(cp) = serde_json::from_str::<Checkpoint>(&json) {
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

    /// Check if a session has a recoverable checkpoint.
    pub fn has_checkpoint(&self, session_id: &str) -> bool {
        let latest_path = self.checkpoint_dir.join(format!("{}_latest.json", session_id));
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

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn sample_checkpoint(session: &str, step: usize) -> Checkpoint {
        Checkpoint {
            id: format!("{}-{}", session, step),
            agent_name: "test-agent".into(),
            session_id: session.into(),
            step,
            messages: vec![
                CheckpointMessage { role: "user".into(), content: "Hello".into() },
                CheckpointMessage { role: "assistant".into(), content: "Hi there".into() },
            ],
            tool_calls: vec![
                ToolCallRecord {
                    tool_name: "search".into(),
                    arguments: "query".into(),
                    result: Some("found it".into()),
                    success: true,
                },
            ],
            partial_results: vec!["partial output".into()],
            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
            status: CheckpointStatus::InProgress,
        }
    }

    #[test]
    fn test_save_and_load() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        let cp = sample_checkpoint("sess1", 0);

        mgr.save(&cp).unwrap();
        let loaded = mgr.load_latest("sess1").unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.session_id, "sess1");
        assert_eq!(loaded.step, 0);
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.tool_calls.len(), 1);
    }

    #[test]
    fn test_load_nonexistent() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        let loaded = mgr.load_latest("nonexistent").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_multiple_steps() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();

        mgr.save(&sample_checkpoint("sess1", 0)).unwrap();
        mgr.save(&sample_checkpoint("sess1", 1)).unwrap();
        mgr.save(&sample_checkpoint("sess1", 2)).unwrap();

        let latest = mgr.load_latest("sess1").unwrap().unwrap();
        assert_eq!(latest.step, 2, "latest should be step 2");

        let all = mgr.list_checkpoints("sess1").unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].step, 0);
        assert_eq!(all[2].step, 2);
    }

    #[test]
    fn test_cleanup() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();

        mgr.save(&sample_checkpoint("sess1", 0)).unwrap();
        mgr.save(&sample_checkpoint("sess1", 1)).unwrap();
        assert!(mgr.has_checkpoint("sess1"));

        let removed = mgr.cleanup("sess1").unwrap();
        assert!(removed >= 2);
        assert!(!mgr.has_checkpoint("sess1"));
    }

    #[test]
    fn test_separate_sessions() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();

        mgr.save(&sample_checkpoint("sess1", 0)).unwrap();
        mgr.save(&sample_checkpoint("sess2", 0)).unwrap();

        assert!(mgr.has_checkpoint("sess1"));
        assert!(mgr.has_checkpoint("sess2"));

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

        let loaded = mgr.load_latest("sess1").unwrap().unwrap();
        assert_eq!(loaded.status, CheckpointStatus::Failed("OOM".into()));
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
        cp.messages.push(CheckpointMessage {
            role: "system".into(),
            content: "follow instructions".into(),
        });

        mgr.save(&cp).unwrap();
        let loaded = mgr.load_latest("sess-restore").unwrap().unwrap();

        assert_eq!(loaded.id, cp.id);
        assert_eq!(loaded.agent_name, cp.agent_name);
        assert_eq!(loaded.session_id, cp.session_id);
        assert_eq!(loaded.step, cp.step);
        assert_eq!(loaded.messages, cp.messages);
        assert_eq!(loaded.tool_calls, cp.tool_calls);
        assert_eq!(loaded.partial_results, cp.partial_results);
        assert_eq!(loaded.timestamp, cp.timestamp);
        assert_eq!(loaded.status, cp.status);
    }

    #[test]
    fn test_checkpoint_status_completed_and_halted_round_trip() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();

        let mut completed = sample_checkpoint("sess-status", 0);
        completed.status = CheckpointStatus::Completed;
        mgr.save(&completed).unwrap();
        let loaded = mgr.load_latest("sess-status").unwrap().unwrap();
        assert_eq!(loaded.status, CheckpointStatus::Completed);

        let mut halted = sample_checkpoint("sess-status", 1);
        halted.status = CheckpointStatus::Halted("loop detected".into());
        mgr.save(&halted).unwrap();
        let loaded = mgr.load_latest("sess-status").unwrap().unwrap();
        assert_eq!(
            loaded.status,
            CheckpointStatus::Halted("loop detected".into())
        );
    }

    #[test]
    fn new_fails_when_checkpoint_path_is_a_file() {
        let dir = temp_dir();
        let file_path = dir.path().join("not_a_dir");
        std::fs::write(&file_path, "blocking file").unwrap();

        let err = match CheckpointManager::new(&file_path) {
            Err(err) => err,
            Ok(_) => panic!("expected checkpoint manager creation to fail"),
        };
        assert!(
            matches!(
                err.kind(),
                std::io::ErrorKind::NotADirectory
                    | std::io::ErrorKind::PermissionDenied
                    | std::io::ErrorKind::AlreadyExists
            ),
            "unexpected error kind: {:?}",
            err.kind()
        );
    }

    #[test]
    fn load_latest_invalid_json_returns_invalid_data() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        let session = "bad-json";

        let checkpoint_path = dir.path().join(format!("{session}_0.json"));
        std::fs::write(&checkpoint_path, "not valid json").unwrap();
        let latest_path = dir.path().join(format!("{session}_latest.json"));
        std::fs::write(&latest_path, format!("{session}_0.json")).unwrap();

        let err = mgr.load_latest(session).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn load_latest_stale_pointer_returns_none() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        let session = "stale-pointer";
        let latest_path = dir.path().join(format!("{session}_latest.json"));
        std::fs::write(&latest_path, "missing_checkpoint_file.json").unwrap();

        assert!(mgr.load_latest(session).unwrap().is_none());
    }

    #[test]
    fn list_checkpoints_skips_corrupt_entries() {
        let dir = temp_dir();
        let mgr = CheckpointManager::new(dir.path()).unwrap();
        let session = "sess-skip";

        mgr.save(&sample_checkpoint(session, 0)).unwrap();
        mgr.save(&sample_checkpoint(session, 1)).unwrap();

        let corrupt = dir.path().join(format!("{session}_corrupt.json"));
        std::fs::write(corrupt, "{not valid json").unwrap();

        let listed = mgr.list_checkpoints(session).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].step, 0);
        assert_eq!(listed[1].step, 1);
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
        let dir = mgr.checkpoint_dir.to_string_lossy();
        assert!(
            dir.ends_with("checkpoints"),
            "expected checkpoints subdirectory, got {dir}"
        );
    }
}
