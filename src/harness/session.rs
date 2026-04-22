use crate::providers::types::Message;
use crate::runtime::types::{
    ApprovalRequest, BackgroundJob, ComposerState, RouterIntent, WorkspacePreflight,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub task: String,
    pub messages: Vec<Message>,
    pub tool_budget_used: usize,
    pub turn_count: usize,
    pub status: SessionStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Paused,
    Completed,
    Failed,
}

pub struct SessionStore {
    root: std::path::PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMetadata {
    pub session_id: String,
    pub workspace_root: String,
    pub title: String,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    pub last_active_at: DateTime<Utc>,
    pub router_intent: RouterIntent,
    pub pending_approvals: usize,
    pub background_jobs: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub metadata: SessionMetadata,
    pub transcript: Vec<TranscriptEntry>,
    #[serde(default)]
    pub messages: Vec<Message>,
    pub approvals: Vec<ApprovalRequest>,
    pub background_jobs: Vec<BackgroundJob>,
    pub preflight: WorkspacePreflight,
    pub composer: ComposerState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSelection {
    New,
    Existing(SessionMetadata),
}

impl SessionStore {
    pub fn new(workspace_root: &Path) -> Self {
        Self {
            root: workspace_root.join(".charm"),
        }
    }

    pub fn save(&self, session: &Session) -> anyhow::Result<()> {
        let path = self.legacy_path();
        let json = serde_json::to_string_pretty(session)?;
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load(&self) -> anyhow::Result<Option<Session>> {
        let path = self.legacy_path();
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(path)?;
        let session: Session = serde_json::from_str(&raw)?;
        Ok(Some(session))
    }

    pub fn clear(&self) -> anyhow::Result<()> {
        let path = self.legacy_path();
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn save_snapshot(&self, snapshot: &SessionSnapshot) -> anyhow::Result<()> {
        let session_dir = self.session_dir(&snapshot.metadata.session_id);
        std::fs::create_dir_all(&session_dir)?;
        std::fs::write(
            session_dir.join("metadata.json"),
            serde_json::to_string_pretty(&snapshot.metadata)?,
        )?;
        std::fs::write(
            session_dir.join("transcript.json"),
            serde_json::to_string_pretty(&snapshot.transcript)?,
        )?;
        std::fs::write(
            session_dir.join("messages.json"),
            serde_json::to_string_pretty(&snapshot.messages)?,
        )?;
        std::fs::write(
            session_dir.join("approvals.json"),
            serde_json::to_string_pretty(&snapshot.approvals)?,
        )?;
        std::fs::write(
            session_dir.join("background-jobs.json"),
            serde_json::to_string_pretty(&snapshot.background_jobs)?,
        )?;
        std::fs::write(
            session_dir.join("preflight.json"),
            serde_json::to_string_pretty(&snapshot.preflight)?,
        )?;
        std::fs::write(
            session_dir.join("composer.json"),
            serde_json::to_string_pretty(&snapshot.composer)?,
        )?;
        Ok(())
    }

    pub fn load_snapshot(&self, session_id: &str) -> anyhow::Result<Option<SessionSnapshot>> {
        self.migrate_legacy_if_needed()?;
        let session_dir = self.session_dir(session_id);
        if !session_dir.exists() {
            return Ok(None);
        }
        let metadata: SessionMetadata =
            serde_json::from_str(&std::fs::read_to_string(session_dir.join("metadata.json"))?)?;
        let transcript: Vec<TranscriptEntry> = serde_json::from_str(&std::fs::read_to_string(
            session_dir.join("transcript.json"),
        )?)?;
        let messages: Vec<Message> =
            serde_json::from_str(&std::fs::read_to_string(session_dir.join("messages.json"))?)?;
        let approvals: Vec<ApprovalRequest> = serde_json::from_str(&std::fs::read_to_string(
            session_dir.join("approvals.json"),
        )?)?;
        let background_jobs: Vec<BackgroundJob> = serde_json::from_str(&std::fs::read_to_string(
            session_dir.join("background-jobs.json"),
        )?)?;
        let preflight: WorkspacePreflight = serde_json::from_str(&std::fs::read_to_string(
            session_dir.join("preflight.json"),
        )?)?;
        let composer: ComposerState =
            serde_json::from_str(&std::fs::read_to_string(session_dir.join("composer.json"))?)?;

        Ok(Some(SessionSnapshot {
            metadata,
            transcript,
            messages,
            approvals,
            background_jobs,
            preflight,
            composer,
        }))
    }

    pub fn list_metadata(&self) -> anyhow::Result<Vec<SessionMetadata>> {
        self.migrate_legacy_if_needed()?;
        let sessions_dir = self.sessions_dir();
        if !sessions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut entries = Vec::new();
        for entry in std::fs::read_dir(sessions_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let metadata_path = entry.path().join("metadata.json");
            if !metadata_path.exists() {
                continue;
            }
            let metadata: SessionMetadata =
                serde_json::from_str(&std::fs::read_to_string(metadata_path)?)?;
            entries.push(metadata);
        }
        entries.sort_by(|a, b| b.last_active_at.cmp(&a.last_active_at));
        Ok(entries)
    }

    pub fn smart_continue(&self) -> anyhow::Result<SessionSelection> {
        let all = self.list_metadata()?;
        let preferred = all
            .iter()
            .find(|meta| {
                matches!(meta.status, SessionStatus::Active | SessionStatus::Paused)
                    || meta.pending_approvals > 0
                    || meta.background_jobs > 0
            })
            .cloned()
            .or_else(|| all.first().cloned());

        match preferred {
            Some(metadata) => Ok(SessionSelection::Existing(metadata)),
            None => Ok(SessionSelection::New),
        }
    }

    fn legacy_path(&self) -> std::path::PathBuf {
        self.root.join("session.json")
    }

    fn sessions_dir(&self) -> std::path::PathBuf {
        self.root.join("sessions")
    }

    fn session_dir(&self, session_id: &str) -> std::path::PathBuf {
        self.sessions_dir().join(session_id)
    }

    fn migrate_legacy_if_needed(&self) -> anyhow::Result<()> {
        let legacy_path = self.legacy_path();
        if !legacy_path.exists() {
            return Ok(());
        }

        let legacy: Session = serde_json::from_str(&std::fs::read_to_string(&legacy_path)?)?;
        let target_dir = self.session_dir(&legacy.session_id);
        if target_dir.exists() {
            std::fs::remove_file(legacy_path)?;
            return Ok(());
        }

        let snapshot = SessionSnapshot {
            metadata: SessionMetadata {
                session_id: legacy.session_id.clone(),
                workspace_root: self
                    .root
                    .parent()
                    .unwrap_or(&self.root)
                    .display()
                    .to_string(),
                title: if legacy.task.trim().is_empty() {
                    "Interactive session".to_string()
                } else {
                    legacy.task.clone()
                },
                status: legacy.status.clone(),
                created_at: Utc::now(),
                last_active_at: Utc::now(),
                router_intent: RouterIntent::Explore,
                pending_approvals: 0,
                background_jobs: 0,
            },
            transcript: legacy
                .messages
                .clone()
                .into_iter()
                .filter_map(|message| {
                    message.content.map(|content| TranscriptEntry {
                        role: message.role,
                        content,
                        timestamp: Utc::now(),
                    })
                })
                .collect(),
            messages: legacy.messages,
            approvals: Vec::new(),
            background_jobs: Vec::new(),
            preflight: WorkspacePreflight::default(),
            composer: ComposerState::default(),
        };
        self.save_snapshot(&snapshot)?;
        std::fs::remove_file(legacy_path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_snapshot(root: &Path, session_id: &str, status: SessionStatus) -> SessionSnapshot {
        let ts = "2026-04-22T00:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("fixed timestamp");
        SessionSnapshot {
            metadata: SessionMetadata {
                session_id: session_id.to_string(),
                workspace_root: root.display().to_string(),
                title: "demo".to_string(),
                status,
                created_at: ts,
                last_active_at: ts,
                router_intent: RouterIntent::Explore,
                pending_approvals: 0,
                background_jobs: 0,
            },
            transcript: vec![TranscriptEntry {
                role: "user".to_string(),
                content: "hello".to_string(),
                timestamp: ts,
            }],
            messages: vec![Message {
                role: "user".to_string(),
                content: Some("hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            }],
            approvals: Vec::new(),
            background_jobs: Vec::new(),
            preflight: WorkspacePreflight::default(),
            composer: ComposerState::default(),
        }
    }

    #[test]
    fn smart_continue_prefers_active_session() {
        let dir = tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        store
            .save_snapshot(&sample_snapshot(
                dir.path(),
                "older-complete",
                SessionStatus::Completed,
            ))
            .unwrap();
        store
            .save_snapshot(&sample_snapshot(
                dir.path(),
                "active",
                SessionStatus::Active,
            ))
            .unwrap();

        let selection = store.smart_continue().unwrap();
        assert_eq!(
            selection,
            SessionSelection::Existing(SessionMetadata {
                session_id: "active".to_string(),
                workspace_root: dir.path().display().to_string(),
                title: "demo".to_string(),
                status: SessionStatus::Active,
                created_at: "2026-04-22T00:00:00Z".parse().unwrap(),
                last_active_at: "2026-04-22T00:00:00Z".parse().unwrap(),
                router_intent: RouterIntent::Explore,
                pending_approvals: 0,
                background_jobs: 0,
            })
        );
    }

    #[test]
    fn legacy_session_is_migrated_into_multi_session_store() {
        let dir = tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        store
            .save(&Session {
                session_id: "legacy".to_string(),
                task: "legacy task".to_string(),
                messages: vec![Message {
                    role: "user".to_string(),
                    content: Some("legacy task".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning: None,
                    reasoning_details: None,
                }],
                tool_budget_used: 1,
                turn_count: 1,
                status: SessionStatus::Active,
            })
            .unwrap();

        let selection = store.smart_continue().unwrap();
        let SessionSelection::Existing(meta) = selection else {
            panic!("expected migrated session");
        };
        assert_eq!(meta.session_id, "legacy");

        let migrated = store.load_snapshot("legacy").unwrap().expect("snapshot");
        assert_eq!(migrated.transcript.len(), 1);
        assert_eq!(migrated.transcript[0].content, "legacy task");
    }
}
