use crate::core::{CheckpointRecord, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CheckpointStore {
    pub records: Vec<CheckpointRecord>,
}

impl CheckpointStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, record: CheckpointRecord) {
        self.records.push(record);
    }

    pub fn resolve_commit_sha(&self, id: &str) -> Option<String> {
        self.records
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.commit_sha.clone())
    }
}

pub struct CheckpointManager {
    repo: git2::Repository,
    store_path: std::path::PathBuf,
    store: CheckpointStore,
}

impl CheckpointManager {
    pub fn new(repo_root: &Path) -> anyhow::Result<Self> {
        let repo = git2::Repository::open(repo_root)?;
        let store_path = repo_root.join(".charm").join("checkpoints.json");
        let store = if store_path.exists() {
            std::fs::read_to_string(&store_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            CheckpointStore::new()
        };
        Ok(Self {
            repo,
            store_path,
            store,
        })
    }

    fn save_store(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.store_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.store_path, serde_json::to_string_pretty(&self.store)?)?;
        Ok(())
    }

    pub fn create(&mut self, args: Value) -> anyhow::Result<ToolResult> {
        let name = args["name"].as_str().unwrap_or("checkpoint");
        let scope = args["scope"].as_str().unwrap_or("manual");

        let mut index = self.repo.index()?;
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;

        let tree_id = index.write_tree()?;
        let tree = self.repo.find_tree(tree_id)?;
        let sig = git2::Signature::now("charm", "charm@local")?;
        let parent = self.repo.head()?.peel_to_commit()?;

        let commit_id = self.repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &format!("[charm-checkpoint] {}", name),
            &tree,
            &[&parent],
        )?;

        let checkpoint = CheckpointRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            scope: scope.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            commit_sha: Some(commit_id.to_string()),
        };

        self.store.add(checkpoint.clone());
        self.save_store()?;

        Ok(ToolResult {
            success: true,
            output: format!(
                "Checkpoint created: {} ({})",
                checkpoint.id,
                &commit_id.to_string()[..7]
            ),
            error: None,
            metadata: Some(serde_json::to_value(&checkpoint)?),
        })
    }

    pub fn restore(&mut self, args: Value) -> anyhow::Result<ToolResult> {
        let checkpoint_id = args["checkpoint_id"].as_str().unwrap_or("");

        // Resolve UUID to SHA; fall back to raw SHA for backward compatibility.
        let sha = self
            .store
            .resolve_commit_sha(checkpoint_id)
            .unwrap_or_else(|| checkpoint_id.to_string());

        // Only treat canonical 40-char hex strings as OIDs. Short SHAs must go
        // through git rev-parse style resolution instead of zero-padded OIDs.
        let commit = if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
            self.repo.find_commit(git2::Oid::from_str(&sha)?)?
        } else {
            self.repo.revparse_single(&sha)?.peel_to_commit()?
        };

        let tree = commit.tree()?;
        self.repo.checkout_tree(
            tree.as_object(),
            Some(git2::build::CheckoutBuilder::new().force()),
        )?;
        self.repo.set_head_detached(commit.id())?;

        Ok(ToolResult {
            success: true,
            output: format!("Restored to checkpoint {}", checkpoint_id),
            error: None,
            metadata: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_commit_sha_finds_uuid_match() {
        let mut store = CheckpointStore::new();
        store.add(CheckpointRecord {
            id: "abc-123".to_string(),
            name: "test".to_string(),
            scope: "manual".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            commit_sha: Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string()),
        });

        assert_eq!(
            store.resolve_commit_sha("abc-123"),
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string())
        );
    }

    #[test]
    fn resolve_commit_sha_returns_none_for_unknown_id() {
        let store = CheckpointStore::new();
        assert!(store.resolve_commit_sha("nonexistent").is_none());
    }

    #[test]
    fn restore_resolves_full_sha_and_abbreviated_sha() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();

        let full_sha = {
            let sig = git2::Signature::now("test", "test@test.com").unwrap();
            let mut index = repo.index().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
                .unwrap()
                .to_string()
        };
        let short_sha = &full_sha[..7];

        let store_path = dir.path().join(".charm").join("checkpoints.json");
        let mut mgr = CheckpointManager {
            repo,
            store_path,
            store: CheckpointStore::new(),
        };

        let full_result = mgr
            .restore(serde_json::json!({"checkpoint_id": &full_sha}))
            .unwrap();
        assert!(full_result.success);

        let short_result = mgr
            .restore(serde_json::json!({"checkpoint_id": short_sha}))
            .unwrap();
        assert!(short_result.success);
    }

    #[test]
    fn restore_resolves_uuid_to_sha() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();

        let full_sha = {
            let sig = git2::Signature::now("test", "test@test.com").unwrap();
            let mut index = repo.index().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
                .unwrap()
                .to_string()
        };

        let mut store = CheckpointStore::new();
        store.add(CheckpointRecord {
            id: "uuid-001".to_string(),
            name: "test".to_string(),
            scope: "manual".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            commit_sha: Some(full_sha.clone()),
        });

        let store_path = dir.path().join(".charm").join("checkpoints.json");
        let mut mgr = CheckpointManager {
            repo,
            store_path,
            store,
        };

        let result = mgr
            .restore(serde_json::json!({"checkpoint_id": "uuid-001"}))
            .unwrap();
        assert!(result.success);
    }
}
