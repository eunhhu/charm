use super::types::{BackgroundJob, BackgroundJobKind, BackgroundJobStatus};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct SubAgentReport {
    pub summary: String,
    pub worktree_path: Option<String>,
    pub changed_files: Vec<String>,
    pub turns: usize,
}

impl SubAgentReport {
    pub fn summary_only(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            worktree_path: None,
            changed_files: Vec::new(),
            turns: 0,
        }
    }

    fn metadata(&self) -> serde_json::Value {
        serde_json::json!({
            "worktree_path": self.worktree_path,
            "changed_files": self.changed_files,
            "turns": self.turns,
        })
    }
}

/// Shared state bag used to communicate background job updates from
/// `tokio::spawn` tasks back into `SessionRuntime`. Tasks push the latest
/// snapshot for a job id into this bag; the session runtime drains the bag
/// whenever the main loop gets another chance and emits events.
#[derive(Clone, Default)]
pub struct SubAgentBus {
    inner: Arc<Mutex<HashMap<String, PendingJobUpdate>>>,
}

#[derive(Clone)]
struct PendingJobUpdate {
    session_id: Option<String>,
    job: BackgroundJob,
}

impl SubAgentBus {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn publish(&self, job: BackgroundJob) {
        self.publish_scoped(None, job);
    }

    pub fn publish_for_session(&self, session_id: impl Into<String>, job: BackgroundJob) {
        self.publish_scoped(Some(session_id.into()), job);
    }

    fn publish_scoped(&self, session_id: Option<String>, job: BackgroundJob) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.insert(job.id.clone(), PendingJobUpdate { session_id, job });
        }
    }

    /// Drain all pending jobs. Each job id appears at most once per drain.
    pub fn drain(&self) -> Vec<BackgroundJob> {
        match self.inner.lock() {
            Ok(mut inner) => inner.drain().map(|(_, update)| update.job).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Drain pending updates that belong to one session. Updates for other
    /// sessions remain queued until that session becomes active again.
    pub fn drain_for_session(&self, session_id: &str) -> Vec<BackgroundJob> {
        match self.inner.lock() {
            Ok(mut inner) => {
                let ids = inner
                    .iter()
                    .filter(|(_, update)| update.session_id.as_deref() == Some(session_id))
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                ids.into_iter()
                    .filter_map(|id| inner.remove(&id).map(|update| update.job))
                    .collect()
            }
            Err(_) => Vec::new(),
        }
    }

    /// Return the latest pending job updates without consuming them. UI code
    /// may use this for idle progress rendering, while SessionRuntime remains
    /// the owner that drains and persists canonical updates.
    pub fn snapshot(&self) -> Vec<BackgroundJob> {
        match self.inner.lock() {
            Ok(inner) => inner.values().map(|update| update.job.clone()).collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Spawn a background sub-agent backed by a real executor. The executor owns
/// the task-specific work; this layer only publishes lifecycle updates and
/// keeps SessionRuntime as the canonical persistence owner.
pub fn spawn_executor_subagent<F, Fut>(
    bus: SubAgentBus,
    session_id: String,
    title: String,
    executor: F,
) -> BackgroundJob
where
    F: FnOnce(String) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<SubAgentReport>> + Send + 'static,
{
    let id = Uuid::new_v4().to_string();
    let job = BackgroundJob {
        id: id.clone(),
        title: title.clone(),
        status: BackgroundJobStatus::Queued,
        detail: "Sub-agent queued".to_string(),
        kind: BackgroundJobKind::SubAgent,
        progress: Some(0),
        metadata: None,
    };
    bus.publish_for_session(&session_id, job.clone());

    tokio::spawn(async move {
        bus.publish_for_session(
            &session_id,
            BackgroundJob {
                id: id.clone(),
                title: title.clone(),
                status: BackgroundJobStatus::Running,
                detail: "[running] executing task".to_string(),
                kind: BackgroundJobKind::SubAgent,
                progress: Some(15),
                metadata: None,
            },
        );

        match executor(title.clone()).await {
            Ok(report) => {
                let summary = trim_detail(&report.summary);
                bus.publish_for_session(
                    &session_id,
                    BackgroundJob {
                        id,
                        title,
                        status: BackgroundJobStatus::Completed,
                        detail: summary,
                        kind: BackgroundJobKind::SubAgent,
                        progress: Some(100),
                        metadata: Some(report.metadata()),
                    },
                );
            }
            Err(err) => {
                bus.publish_for_session(
                    &session_id,
                    BackgroundJob {
                        id,
                        title,
                        status: BackgroundJobStatus::Failed,
                        detail: trim_detail(&err.to_string()),
                        kind: BackgroundJobKind::SubAgent,
                        progress: None,
                        metadata: None,
                    },
                );
            }
        }
    });

    job
}

/// Compatibility helper for tests and degraded mode. Production session code
/// should prefer `spawn_executor_subagent`.
pub fn spawn_stub_subagent(bus: SubAgentBus, session_id: String, title: String) -> BackgroundJob {
    spawn_executor_subagent(bus, session_id, title, |task| async move {
        tokio::time::sleep(Duration::from_millis(900)).await;
        Ok(SubAgentReport::summary_only(format!(
            "summary ready for {task}"
        )))
    })
}

fn trim_detail(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "Sub-agent completed without a text summary.".to_string();
    }
    const MAX_DETAIL: usize = 1200;
    if trimmed.len() <= MAX_DETAIL {
        return trimmed.to_string();
    }
    let mut end = MAX_DETAIL;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &trimmed[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_and_drain_round_trips_latest_job_state() {
        let bus = SubAgentBus::new();
        let job = BackgroundJob {
            id: "j-1".to_string(),
            title: "test".to_string(),
            status: BackgroundJobStatus::Running,
            detail: "starting".to_string(),
            kind: BackgroundJobKind::SubAgent,
            progress: Some(10),
            metadata: None,
        };
        bus.publish(job.clone());
        let mut updated = job.clone();
        updated.progress = Some(40);
        bus.publish(updated);

        let drained = bus.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].id, "j-1");
        assert_eq!(drained[0].progress, Some(40));
        assert!(bus.drain().is_empty());
    }

    #[test]
    fn snapshot_does_not_consume_runtime_updates() {
        let bus = SubAgentBus::new();
        bus.publish(BackgroundJob {
            id: "j-1".to_string(),
            title: "test".to_string(),
            status: BackgroundJobStatus::Running,
            detail: "starting".to_string(),
            kind: BackgroundJobKind::SubAgent,
            progress: Some(10),
            metadata: None,
        });

        let snapshot = bus.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].progress, Some(10));

        let drained = bus.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].id, "j-1");
    }

    #[test]
    fn scoped_drain_keeps_updates_in_their_own_session() {
        let bus = SubAgentBus::new();
        bus.publish_for_session(
            "session-a",
            BackgroundJob {
                id: "j-a".to_string(),
                title: "a".to_string(),
                status: BackgroundJobStatus::Running,
                detail: "from a".to_string(),
                kind: BackgroundJobKind::SubAgent,
                progress: Some(20),
                metadata: None,
            },
        );
        bus.publish_for_session(
            "session-b",
            BackgroundJob {
                id: "j-b".to_string(),
                title: "b".to_string(),
                status: BackgroundJobStatus::Completed,
                detail: "from b".to_string(),
                kind: BackgroundJobKind::SubAgent,
                progress: Some(100),
                metadata: None,
            },
        );

        let session_a = bus.drain_for_session("session-a");
        assert_eq!(session_a.len(), 1);
        assert_eq!(session_a[0].id, "j-a");
        assert!(bus.drain_for_session("session-a").is_empty());

        let session_b = bus.drain_for_session("session-b");
        assert_eq!(session_b.len(), 1);
        assert_eq!(session_b[0].id, "j-b");
    }

    #[tokio::test]
    async fn executor_subagent_publishes_executor_summary() {
        let bus = SubAgentBus::new();
        let job = spawn_executor_subagent(
            bus.clone(),
            "session-a".to_string(),
            "audit auth".to_string(),
            |task| async move { Ok(SubAgentReport::summary_only(format!("summary for {task}"))) },
        );

        assert_eq!(job.status, BackgroundJobStatus::Queued);

        let mut completed = None;
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            for update in bus.drain_for_session("session-a") {
                if update.status == BackgroundJobStatus::Completed {
                    completed = Some(update);
                }
            }
            if completed.is_some() {
                break;
            }
        }

        let completed = completed.expect("executor should publish completion");
        assert_eq!(completed.progress, Some(100));
        assert!(completed.detail.contains("summary for audit auth"));
    }
}
