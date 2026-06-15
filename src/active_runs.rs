use parking_lot::RwLock;
use std::collections::HashMap;

const TERMINAL_RUN_RETENTION_SECONDS: i64 = 60;

/// Snapshot of an active agent run for the live dashboard.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ActiveRun {
    pub run_id: String,
    pub tenant_id: String,
    pub agent_name: String,
    pub started_at: i64,
    pub status: String,
    pub current_step: i32,
    pub total_steps: i32,
    pub last_update: i64,
    pub tool_name: Option<String>,
    pub model: Option<String>,
    pub is_catchup: bool,
    pub request_source: Option<String>,
    pub pipeline_id: Option<String>,
    pub schedule_id: Option<String>,
    pub trigger_id: Option<String>,
}

/// Thread-safe registry of in-progress agent runs.
pub struct ActiveRuns {
    runs: RwLock<HashMap<String, ActiveRun>>,
}

impl ActiveRuns {
    pub fn new() -> Self {
        Self {
            runs: RwLock::new(HashMap::new()),
        }
    }

    pub fn start(&self, run: ActiveRun) {
        let mut runs = self.runs.write();
        runs.insert(run.run_id.clone(), run);
    }

    pub fn start_with_catchup(&self, run: ActiveRun, is_catchup: bool) {
        let mut runs = self.runs.write();
        let mut run = run;
        run.is_catchup = is_catchup;
        runs.insert(run.run_id.clone(), run);
    }

    pub fn update(&self, run_id: &str, status: &str, step: i32) {
        let mut runs = self.runs.write();
        if let Some(run) = runs.get_mut(run_id) {
            run.status = status.to_string();
            run.current_step = step;
            run.last_update = chrono::Utc::now().timestamp();
        }
    }

    pub fn update_tool(&self, run_id: &str, tool_name: Option<&str>) {
        let mut runs = self.runs.write();
        if let Some(run) = runs.get_mut(run_id) {
            run.tool_name = tool_name.map(|s| s.to_string());
            run.last_update = chrono::Utc::now().timestamp();
        }
    }

    pub fn update_model(&self, run_id: &str, model: Option<&str>) {
        let mut runs = self.runs.write();
        if let Some(run) = runs.get_mut(run_id) {
            run.model = model.map(|s| s.to_string());
            run.last_update = chrono::Utc::now().timestamp();
        }
    }

    pub fn finish(&self, run_id: &str, status: &str) {
        let mut runs = self.runs.write();
        if let Some(run) = runs.get_mut(run_id) {
            run.status = status.to_string();
            run.last_update = chrono::Utc::now().timestamp();
        }
    }

    pub fn remove(&self, run_id: &str) {
        let mut runs = self.runs.write();
        runs.remove(run_id);
    }

    pub fn list(&self) -> Vec<ActiveRun> {
        let now = chrono::Utc::now().timestamp();
        let mut runs = self.runs.write();
        runs.retain(|_, run| {
            !is_terminal_status(&run.status)
                || now.saturating_sub(run.last_update) <= TERMINAL_RUN_RETENTION_SECONDS
        });
        let mut snapshots: Vec<ActiveRun> = runs.values().cloned().collect();
        snapshots.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then_with(|| left.agent_name.cmp(&right.agent_name))
                .then_with(|| left.run_id.cmp(&right.run_id))
        });
        snapshots
    }
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "error" | "cancelled")
}

impl Default for ActiveRuns {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, status: &str, last_update: i64) -> ActiveRun {
        ActiveRun {
            run_id: id.to_string(),
            tenant_id: "tenant-1".to_string(),
            agent_name: "agent-1".to_string(),
            started_at: last_update,
            status: status.to_string(),
            current_step: 0,
            total_steps: 1,
            last_update,
            tool_name: None,
            model: None,
            is_catchup: false,
            request_source: None,
            pipeline_id: None,
            schedule_id: None,
            trigger_id: None,
        }
    }

    #[test]
    fn active_runs_updates_and_lists_current_runs() {
        let registry = ActiveRuns::new();
        registry.start(run("run-1", "running", chrono::Utc::now().timestamp()));

        registry.update("run-1", "tool_call", 1);
        registry.update_tool("run-1", Some("calendar"));
        registry.update_model("run-1", Some("gpt-4o"));

        let runs = registry.list();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "tool_call");
        assert_eq!(runs[0].current_step, 1);
        assert_eq!(runs[0].tool_name.as_deref(), Some("calendar"));
        assert_eq!(runs[0].model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn active_runs_prunes_old_terminal_runs() {
        let registry = ActiveRuns::new();
        let old = chrono::Utc::now().timestamp() - TERMINAL_RUN_RETENTION_SECONDS - 1;
        registry.start(run("run-1", "completed", old));
        registry.start(run("run-2", "running", old));

        let runs = registry.list();

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "run-2");
    }

    #[test]
    fn active_runs_list_is_deterministically_ordered() {
        let registry = ActiveRuns::new();
        registry.start(run("run-z", "running", 100));
        let mut same_time_a = run("run-b", "running", 200);
        same_time_a.agent_name = "beta".to_string();
        registry.start(same_time_a);
        let mut same_time_b = run("run-a", "running", 200);
        same_time_b.agent_name = "alpha".to_string();
        registry.start(same_time_b);

        let runs = registry.list();

        assert_eq!(
            runs.iter()
                .map(|run| run.run_id.as_str())
                .collect::<Vec<_>>(),
            vec!["run-a", "run-b", "run-z"]
        );
    }

    #[test]
    fn active_runs_preserves_catchup_flag() {
        let registry = ActiveRuns::new();
        registry.start_with_catchup(
            run("run-1", "running", chrono::Utc::now().timestamp()),
            true,
        );

        let runs = registry.list();

        assert_eq!(runs.len(), 1);
        assert!(runs[0].is_catchup);
    }
}
