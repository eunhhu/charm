use crate::providers::types::Message;
use crate::runtime::types::{
    ApprovalRequest, ApprovalStatus, AutonomyLevel, BackgroundJob, BackgroundJobStatus,
    ComposerState, RouterIntent, WorkspacePreflight,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Write `content` to `path` atomically by writing to a temp file in the
/// same directory and then renaming. `std::fs::rename` is atomic on the same
/// filesystem, so readers never see a partially-written file.
fn atomic_write(path: &Path, content: &str) -> anyhow::Result<()> {
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, content)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

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
    #[serde(default = "default_autonomy")]
    pub autonomy_level: AutonomyLevel,
    #[serde(default)]
    pub pinned_model: Option<String>,
}

fn default_autonomy() -> AutonomyLevel {
    AutonomyLevel::Aggressive
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

impl SessionSnapshot {
    /// Trim all collections to their respective caps to prevent unbounded growth
    /// in long-running sessions. Preserves the system prompt in messages and
    /// retains the most recent entries for each collection.
    pub fn trim_to_caps(
        &mut self,
        max_transcript: usize,
        max_messages: usize,
        max_resolved_approvals: usize,
        max_completed_jobs: usize,
    ) {
        self.trim_transcript(max_transcript);
        self.trim_messages(max_messages);
        self.trim_resolved_approvals(max_resolved_approvals);
        self.trim_completed_jobs(max_completed_jobs);
    }

    /// Keep only the most recent transcript entries.
    fn trim_transcript(&mut self, max_entries: usize) {
        if self.transcript.len() > max_entries {
            let drain_count = self.transcript.len() - max_entries;
            self.transcript.drain(0..drain_count);
        }
    }

    /// Keep the system prompt plus the most recent messages, then remove any
    /// orphaned tool messages whose `tool_call_id` is no longer referenced.
    fn trim_messages(&mut self, max_messages: usize) {
        if self.messages.len() <= max_messages {
            return;
        }

        let system_end = if !self.messages.is_empty() && self.messages[0].role == "system" {
            1usize
        } else {
            0usize
        };

        let max_non_system = max_messages.saturating_sub(system_end);
        let non_system_count = self.messages.len() - system_end;

        if non_system_count <= max_non_system {
            return;
        }

        let drain_count = non_system_count - max_non_system;
        self.messages.drain(system_end..system_end + drain_count);
        self.remove_orphaned_tool_messages();
    }

    /// Remove tool-role messages that reference a `tool_call_id` not present
    /// in any assistant message's `tool_calls` within the current message list.
    fn remove_orphaned_tool_messages(&mut self) {
        let known_ids: HashSet<String> = self
            .messages
            .iter()
            .filter_map(|m| m.tool_calls.as_ref())
            .flatten()
            .map(|tc| tc.id.clone())
            .collect();

        self.messages.retain(|m| {
            if m.role == "tool" {
                m.tool_call_id
                    .as_ref()
                    .map(|id| known_ids.contains(id))
                    .unwrap_or(true)
            } else {
                true
            }
        });
    }

    /// Remove the oldest resolved/denied approvals, keeping at most
    /// `max_resolved` non-pending approvals. Pending approvals are never removed.
    fn trim_resolved_approvals(&mut self, max_resolved: usize) {
        let resolved_count = self
            .approvals
            .iter()
            .filter(|a| a.status != ApprovalStatus::Pending)
            .count();

        if resolved_count <= max_resolved {
            return;
        }

        let to_remove = resolved_count - max_resolved;
        let mut removed = 0usize;
        self.approvals.retain(|a| {
            if removed >= to_remove {
                return true;
            }
            if a.status != ApprovalStatus::Pending {
                removed += 1;
                return false;
            }
            true
        });
    }

    /// Remove completed/failed/cancelled background jobs beyond `max_completed`.
    /// Running and queued jobs are always retained.
    fn trim_completed_jobs(&mut self, max_completed: usize) {
        let completed_count = self
            .background_jobs
            .iter()
            .filter(|j| {
                !matches!(
                    j.status,
                    BackgroundJobStatus::Running | BackgroundJobStatus::Queued
                )
            })
            .count();

        if completed_count <= max_completed {
            return;
        }

        let to_remove = completed_count - max_completed;
        let mut removed = 0usize;
        self.background_jobs.retain(|j| {
            if removed >= to_remove {
                return true;
            }
            if !matches!(
                j.status,
                BackgroundJobStatus::Running | BackgroundJobStatus::Queued
            ) {
                removed += 1;
                return false;
            }
            true
        });
    }
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
        atomic_write(&path, &json)?;
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
        atomic_write(
            &session_dir.join("metadata.json"),
            &serde_json::to_string_pretty(&snapshot.metadata)?,
        )?;
        atomic_write(
            &session_dir.join("transcript.json"),
            &serde_json::to_string_pretty(&snapshot.transcript)?,
        )?;
        atomic_write(
            &session_dir.join("messages.json"),
            &serde_json::to_string_pretty(&snapshot.messages)?,
        )?;
        atomic_write(
            &session_dir.join("approvals.json"),
            &serde_json::to_string_pretty(&snapshot.approvals)?,
        )?;
        atomic_write(
            &session_dir.join("background-jobs.json"),
            &serde_json::to_string_pretty(&snapshot.background_jobs)?,
        )?;
        atomic_write(
            &session_dir.join("preflight.json"),
            &serde_json::to_string_pretty(&snapshot.preflight)?,
        )?;
        atomic_write(
            &session_dir.join("composer.json"),
            &serde_json::to_string_pretty(&snapshot.composer)?,
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

        let messages = Self::load_optional(&session_dir, "messages.json").unwrap_or_default();
        let approvals = Self::load_optional(&session_dir, "approvals.json").unwrap_or_default();
        let background_jobs =
            Self::load_optional(&session_dir, "background-jobs.json").unwrap_or_default();
        let preflight = Self::load_optional(&session_dir, "preflight.json")
            .unwrap_or(WorkspacePreflight::default());
        let composer =
            Self::load_optional(&session_dir, "composer.json").unwrap_or(ComposerState::default());

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

    fn load_optional<T>(dir: &Path, filename: &str) -> Option<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let path = dir.join(filename);
        let raw = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&raw).ok()
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
                autonomy_level: default_autonomy(),
                pinned_model: None,
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
                autonomy_level: AutonomyLevel::Aggressive,
                pinned_model: None,
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
                autonomy_level: AutonomyLevel::Aggressive,
                pinned_model: None,
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

    #[test]
    fn load_snapshot_tolerates_missing_optional_files() {
        let dir = tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        store
            .save_snapshot(&sample_snapshot(
                dir.path(),
                "partial",
                SessionStatus::Active,
            ))
            .unwrap();

        let session_dir = dir.path().join(".charm").join("sessions").join("partial");
        std::fs::remove_file(session_dir.join("messages.json")).unwrap();
        std::fs::remove_file(session_dir.join("approvals.json")).unwrap();
        std::fs::remove_file(session_dir.join("background-jobs.json")).unwrap();
        std::fs::remove_file(session_dir.join("preflight.json")).unwrap();
        std::fs::remove_file(session_dir.join("composer.json")).unwrap();

        let loaded = store.load_snapshot("partial").unwrap().expect("snapshot");
        assert!(loaded.messages.is_empty());
        assert!(loaded.approvals.is_empty());
        assert!(loaded.background_jobs.is_empty());
        assert_eq!(loaded.preflight, WorkspacePreflight::default());
        assert_eq!(loaded.composer, ComposerState::default());
    }

    #[test]
    fn load_snapshot_fails_on_missing_metadata() {
        let dir = tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let session_dir = dir.path().join(".charm").join("sessions").join("ghost");
        std::fs::create_dir_all(&session_dir).unwrap();

        assert!(store.load_snapshot("ghost").is_err());
    }

    #[test]
    fn atomic_write_prevents_partial_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.json");
        atomic_write(&path, r#"{"ok":true}"#).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"ok":true}"#);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn trim_transcript_keeps_recent_entries() {
        let dir = tempdir().unwrap();
        let mut snap = sample_snapshot(dir.path(), "trim-test", SessionStatus::Active);
        let ts = Utc::now();
        for i in 0..600 {
            snap.transcript.push(TranscriptEntry {
                role: "user".to_string(),
                content: format!("entry {i}"),
                timestamp: ts,
            });
        }
        assert_eq!(snap.transcript.len(), 601);
        snap.trim_to_caps(500, 128, 20, 20);
        assert_eq!(snap.transcript.len(), 500);
        assert_eq!(snap.transcript.first().unwrap().content, "entry 100");
        assert_eq!(snap.transcript.last().unwrap().content, "entry 599");
    }

    #[test]
    fn trim_messages_preserves_system_prompt() {
        let dir = tempdir().unwrap();
        let mut snap = sample_snapshot(dir.path(), "msg-trim", SessionStatus::Active);
        snap.messages.insert(
            0,
            Message {
                role: "system".to_string(),
                content: Some("system prompt".to_string()),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            },
        );
        for i in 0..200 {
            snap.messages.push(Message {
                role: "user".to_string(),
                content: Some(format!("msg {i}")),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            });
        }
        assert_eq!(snap.messages.len(), 202);
        snap.trim_to_caps(500, 64, 20, 20);
        assert!(snap.messages.len() <= 64);
        assert_eq!(snap.messages[0].role, "system");
        assert_eq!(snap.messages[0].content.as_deref(), Some("system prompt"));
    }

    #[test]
    fn trim_messages_removes_orphaned_tool_responses() {
        let dir = tempdir().unwrap();
        let mut snap = sample_snapshot(dir.path(), "orphan-test", SessionStatus::Active);
        snap.messages.insert(
            0,
            Message {
                role: "system".to_string(),
                content: Some("system".to_string()),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            },
        );
        use crate::providers::types::{FunctionCall, ToolCallBlock};
        snap.messages.push(Message {
            role: "assistant".to_string(),
            content: Some("use tool".to_string()),
            tool_calls: Some(vec![ToolCallBlock {
                id: "call-1".to_string(),
                r#type: "function".to_string(),
                function: FunctionCall {
                    name: "read_range".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        });
        snap.messages.push(Message {
            role: "tool".to_string(),
            content: Some("result".to_string()),
            tool_calls: None,
            tool_call_id: Some("call-1".to_string()),
            reasoning: None,
            reasoning_details: None,
        });
        for i in 0..200 {
            snap.messages.push(Message {
                role: "user".to_string(),
                content: Some(format!("msg {i}")),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            });
        }
        snap.trim_to_caps(500, 10, 20, 20);
        for msg in &snap.messages {
            if msg.role == "tool" {
                let tc_id = msg.tool_call_id.as_ref().unwrap();
                let known_ids: std::collections::HashSet<String> = snap
                    .messages
                    .iter()
                    .filter_map(|m| m.tool_calls.as_ref())
                    .flatten()
                    .map(|tc| tc.id.clone())
                    .collect();
                assert!(known_ids.contains(tc_id), "orphaned tool message remains");
            }
        }
    }

    #[test]
    fn trim_approvals_keeps_pending_and_recent_resolved() {
        let dir = tempdir().unwrap();
        let mut snap = sample_snapshot(dir.path(), "approval-trim", SessionStatus::Active);
        let ts = Utc::now();
        for i in 0..30 {
            snap.approvals.push(ApprovalRequest {
                id: format!("resolved-{i}"),
                tool_name: "run_command".to_string(),
                summary: format!("resolved {i}"),
                risk: crate::core::RiskClass::SafeExec,
                status: ApprovalStatus::Approved,
                created_at: ts,
                tool_arguments: None,
                tool_call_id: None,
            });
        }
        snap.approvals.push(ApprovalRequest {
            id: "pending-1".to_string(),
            tool_name: "run_command".to_string(),
            summary: "pending".to_string(),
            risk: crate::core::RiskClass::Destructive,
            status: ApprovalStatus::Pending,
            created_at: ts,
            tool_arguments: None,
            tool_call_id: None,
        });
        assert_eq!(snap.approvals.len(), 31);
        snap.trim_to_caps(500, 128, 10, 20);
        assert_eq!(snap.approvals.len(), 11);
        assert!(snap.approvals.iter().any(|a| a.id == "pending-1"));
        let resolved = snap
            .approvals
            .iter()
            .filter(|a| a.status == ApprovalStatus::Approved)
            .count();
        assert_eq!(resolved, 10);
    }

    #[test]
    fn trim_jobs_keeps_running_and_recent_completed() {
        let dir = tempdir().unwrap();
        let mut snap = sample_snapshot(dir.path(), "job-trim", SessionStatus::Active);
        for i in 0..30 {
            snap.background_jobs.push(BackgroundJob {
                id: format!("completed-{i}"),
                title: format!("completed {i}"),
                status: BackgroundJobStatus::Completed,
                detail: String::new(),
                kind: crate::runtime::types::BackgroundJobKind::Command,
                progress: None,
                metadata: None,
            });
        }
        snap.background_jobs.push(BackgroundJob {
            id: "running-1".to_string(),
            title: "running".to_string(),
            status: BackgroundJobStatus::Running,
            detail: String::new(),
            kind: crate::runtime::types::BackgroundJobKind::Command,
            progress: None,
            metadata: None,
        });
        assert_eq!(snap.background_jobs.len(), 31);
        snap.trim_to_caps(500, 128, 20, 10);
        assert_eq!(snap.background_jobs.len(), 11);
        assert!(snap.background_jobs.iter().any(|j| j.id == "running-1"));
        let completed = snap
            .background_jobs
            .iter()
            .filter(|j| j.status == BackgroundJobStatus::Completed)
            .count();
        assert_eq!(completed, 10);
    }
}
