use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEntry {
    pub id: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub event: String,
    pub timestamp: DateTime<Utc>,
    pub payload: Value,
}

#[derive(Debug, Clone)]
pub struct AgentTraceStore {
    root: PathBuf,
    session_id: String,
}

impl AgentTraceStore {
    pub fn new(workspace_root: &Path, session_id: impl Into<String>) -> Self {
        Self {
            root: workspace_root.join(".charm").join("traces"),
            session_id: session_id.into(),
        }
    }

    pub fn for_session(&self, session_id: impl Into<String>) -> Self {
        Self {
            root: self.root.clone(),
            session_id: session_id.into(),
        }
    }

    pub fn trace_path(&self) -> PathBuf {
        self.root.join(format!("{}.jsonl", self.session_id))
    }

    pub fn append(
        &self,
        turn_id: Option<&str>,
        event: impl Into<String>,
        payload: Value,
    ) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let entry = TraceEntry {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: self.session_id.clone(),
            turn_id: turn_id.map(str::to_string),
            event: event.into(),
            timestamp: Utc::now(),
            payload,
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.trace_path())?;
        writeln!(file, "{}", serde_json::to_string(&entry)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn append_writes_jsonl_trace_entry() {
        let dir = tempdir().unwrap();
        let store = AgentTraceStore::new(dir.path(), "session-1");

        store
            .append(
                Some("turn-1"),
                "task_contract",
                serde_json::json!({"objective": "test"}),
            )
            .unwrap();

        let raw = std::fs::read_to_string(store.trace_path()).unwrap();
        assert!(raw.contains("\"session_id\":\"session-1\""));
        assert!(raw.contains("\"turn_id\":\"turn-1\""));
        assert!(raw.contains("\"event\":\"task_contract\""));
    }
}
