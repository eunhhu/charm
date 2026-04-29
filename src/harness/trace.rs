use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::BufRead;
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

    pub fn read_recent(&self, limit: usize) -> anyhow::Result<Vec<TraceEntry>> {
        if limit == 0 || !self.trace_path().exists() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(self.trace_path())?;
        let mut entries = Vec::new();
        for line in std::io::BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: TraceEntry = serde_json::from_str(&line)?;
            entries.push(entry);
        }
        let start = entries.len().saturating_sub(limit);
        Ok(entries.split_off(start))
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

    #[test]
    fn read_recent_returns_recent_trace_entries_in_chronological_order() {
        let dir = tempdir().unwrap();
        let store = AgentTraceStore::new(dir.path(), "session-1");

        store
            .append(None, "first", serde_json::json!({"idx": 1}))
            .unwrap();
        store
            .append(None, "second", serde_json::json!({"idx": 2}))
            .unwrap();
        store
            .append(None, "third", serde_json::json!({"idx": 3}))
            .unwrap();

        let entries = store.read_recent(2).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].event, "second");
        assert_eq!(entries[1].event, "third");
    }
}
