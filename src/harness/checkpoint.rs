use crate::core::{CheckpointRecord, ToolResult};
use serde_json::Value;
use std::path::Path;

pub struct CheckpointManager {
    repo: git2::Repository,
}

impl CheckpointManager {
    pub fn new(repo_root: &Path) -> anyhow::Result<Self> {
        let repo = git2::Repository::open(repo_root)?;
        Ok(Self { repo })
    }

    pub fn create(&self, args: Value) -> anyhow::Result<ToolResult> {
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

    pub fn restore(&self, args: Value) -> anyhow::Result<ToolResult> {
        let checkpoint_id = args["checkpoint_id"].as_str().unwrap_or("");

        let commit = self
            .repo
            .find_commit(git2::Oid::from_str(checkpoint_id)?)
            .or_else(|_| {
                self.repo.find_commit(git2::Oid::from_str(
                    &checkpoint_id.chars().take(7).collect::<String>(),
                )?)
            })?;

        let tree = commit.tree()?;
        self.repo.checkout_tree(
            &tree.as_object(),
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
