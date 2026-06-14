use std::collections::HashMap;
use std::sync::RwLock;

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
        let mut runs = self.runs.write().expect("active runs lock poisoned");
        runs.insert(run.run_id.clone(), run);
    }

    pub fn start_with_catchup(&self, run: ActiveRun, is_catchup: bool) {
        let mut runs = self.runs.write().expect("active runs lock poisoned");
        let mut run = run;
        run.is_catchup = is_catchup;
        runs.insert(run.run_id.clone(), run);
    }

    pub fn update(&self, run_id: &str, status: &str, step: i32) {
        let mut runs = self.runs.write().expect("active runs lock poisoned");
        if let Some(run) = runs.get_mut(run_id) {
            run.status = status.to_string();
            run.current_step = step;
            run.last_update = chrono::Utc::now().timestamp();
        }
    }

    pub fn update_tool(&self, run_id: &str, tool_name: Option<&str>) {
        let mut runs = self.runs.write().expect("active runs lock poisoned");
        if let Some(run) = runs.get_mut(run_id) {
            run.tool_name = tool_name.map(|s| s.to_string());
            run.last_update = chrono::Utc::now().timestamp();
        }
    }

    pub fn update_model(&self, run_id: &str, model: Option<&str>) {
        let mut runs = self.runs.write().expect("active runs lock poisoned");
        if let Some(run) = runs.get_mut(run_id) {
            run.model = model.map(|s| s.to_string());
            run.last_update = chrono::Utc::now().timestamp();
        }
    }

    pub fn finish(&self, run_id: &str, status: &str) {
        let mut runs = self.runs.write().expect("active runs lock poisoned");
        if let Some(run) = runs.get_mut(run_id) {
            run.status = status.to_string();
            run.last_update = chrono::Utc::now().timestamp();
        }
    }

    pub fn remove(&self, run_id: &str) {
        let mut runs = self.runs.write().expect("active runs lock poisoned");
        runs.remove(run_id);
    }

    pub fn list(&self) -> Vec<ActiveRun> {
        let now = chrono::Utc::now().timestamp();
        let mut runs = self.runs.write().expect("active runs lock poisoned");
        runs.retain(|_, run| {
            !is_terminal_status(&run.status)
                || now.saturating_sub(run.last_update) <= TERMINAL_RUN_RETENTION_SECONDS
        });
        runs.values().cloned().collect()
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
