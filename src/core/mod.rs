use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum ToolCall {
    ReadRange {
        file_path: String,
        #[serde(default)]
        offset: Option<usize>,
        #[serde(default)]
        limit: Option<usize>,
    },
    ReadSymbol {
        file_path: String,
        symbol_name: String,
    },
    GrepSearch {
        pattern: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        include: Option<String>,
        #[serde(default = "default_output_mode")]
        output_mode: OutputMode,
    },
    GlobSearch {
        pattern: String,
        #[serde(default)]
        path: Option<String>,
    },
    ListDir {
        dir_path: String,
    },
    SemanticSearch {
        query: String,
        #[serde(default)]
        top_k: Option<usize>,
        #[serde(default)]
        expand_full: Option<bool>,
    },
    ParallelSearch {
        query: String,
        #[serde(default)]
        top_k: Option<usize>,
    },
    EditPatch {
        file_path: String,
        old_string: String,
        new_string: String,
    },
    WriteFile {
        file_path: String,
        content: String,
    },
    RunCommand {
        command: String,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default = "default_true")]
        blocking: bool,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default = "default_risk_class")]
        risk_class: RiskClass,
    },
    PollCommand {
        command_id: String,
        #[serde(default = "default_output_priority")]
        output_priority: OutputPriority,
        #[serde(default)]
        max_lines: Option<usize>,
    },
    CancelCommand {
        command_id: String,
    },
    PlanUpdate {
        #[serde(default)]
        objective: Option<String>,
        #[serde(default)]
        current_phase: Option<String>,
        #[serde(default)]
        completed_steps: Option<Vec<String>>,
        #[serde(default)]
        blocked_steps: Option<Vec<String>>,
        #[serde(default)]
        notes: Option<String>,
    },
    CheckpointCreate {
        name: String,
        #[serde(default = "default_checkpoint_scope")]
        scope: CheckpointScope,
    },
    CheckpointRestore {
        checkpoint_id: String,
    },
    MemoryStage {
        scope: MemoryScope,
        category: String,
        content: String,
    },
    MemoryCommit {
        memory_ids: Vec<String>,
    },
}

fn default_true() -> bool {
    true
}
fn default_output_mode() -> OutputMode {
    OutputMode::Content
}
fn default_risk_class() -> RiskClass {
    RiskClass::SafeExec
}
fn default_output_priority() -> OutputPriority {
    OutputPriority::Bottom
}
fn default_checkpoint_scope() -> CheckpointScope {
    CheckpointScope::Manual
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    Content,
    FilesWithMatches,
    Count,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    SafeRead,
    SafeExec,
    StatefulExec,
    Destructive,
    ExternalSideEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputPriority {
    Top,
    Bottom,
    Split,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointScope {
    Auto,
    Phase,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Session,
    Project,
    User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandStatus {
    pub command_id: String,
    pub pid: Option<u32>,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub root_path: String,
    pub branch: String,
    pub dirty_files: Vec<String>,
    pub open_files: Vec<OpenFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenFile {
    pub path: String,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

pub fn detect_workspace(root: &Path) -> anyhow::Result<WorkspaceState> {
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(root)
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_else(|| "unknown".to_string())
        .trim()
        .to_string();

    let dirty_files = std::process::Command::new("git")
        .args(["status", "--short"])
        .current_dir(root)
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();

    Ok(WorkspaceState {
        root_path: root.display().to_string(),
        branch,
        dirty_files,
        open_files: Vec::new(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub session_id: String,
    pub workspace: WorkspaceState,
    pub recent_edits: Vec<EditRecord>,
    pub recent_commands: Vec<CommandRecord>,
    pub failing_diagnostics: Vec<String>,
    pub active_plan_item: Option<String>,
    pub memories: Memories,
    pub checkpoints: Vec<CheckpointRecord>,
    pub pending_command: Option<CommandStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditRecord {
    pub file_path: String,
    pub timestamp: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRecord {
    pub command: String,
    pub timestamp: String,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memories {
    pub session: Vec<MemoryEntry>,
    pub project: Vec<ApprovedMemoryEntry>,
    pub user: Vec<ApprovedMemoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub category: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedMemoryEntry {
    pub id: String,
    pub category: String,
    pub content: String,
    pub approved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRecord {
    pub id: String,
    pub name: String,
    pub scope: String,
    pub created_at: String,
    pub commit_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextStack {
    pub system_prompt: String,
    pub rules: Vec<String>,
    pub approved_memories: Vec<String>,
    pub plan_artifact: String,
    pub todo_list: Vec<String>,
    pub workspace_header: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressState {
    pub current_objective: String,
    pub active_substep: String,
    pub waiting_reason: Option<String>,
    pub changed_files: Vec<String>,
    pub verification_status: VerificationStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Pending,
    Passed,
    Failed,
}

/// Resolves a user-supplied path against the workspace root (`cwd`), ensuring
/// the result cannot escape the workspace boundary.
///
/// - Relative paths are joined with `cwd`.
/// - Absolute paths are accepted only if they resolve inside the workspace.
/// - `..` traversal that escapes the workspace is rejected.
/// - Symlinks that resolve outside the workspace are rejected.
///
/// For existing paths the result is fully canonicalized. For not-yet-existing
/// write targets the longest existing prefix is canonicalized and the remaining
/// components are appended, so the boundary check still covers `../../` attacks.
pub fn resolve_workspace_path(path: &str, cwd: &Path) -> Result<PathBuf, String> {
    if path.is_empty() {
        return Err("path must not be empty".to_string());
    }

    let resolved = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };

    let canonical_cwd = cwd.canonicalize().map_err(|e| {
        format!(
            "Cannot canonicalize workspace root '{}': {}",
            cwd.display(),
            e
        )
    })?;

    if resolved.exists() {
        let canonical = resolved
            .canonicalize()
            .map_err(|e| format!("Cannot canonicalize path '{}': {}", resolved.display(), e))?;
        if !canonical.starts_with(&canonical_cwd) {
            return Err(format!(
                "Path '{}' resolves to '{}' which is outside the workspace root '{}'",
                path,
                canonical.display(),
                canonical_cwd.display()
            ));
        }
        Ok(canonical)
    } else {
        if Path::new(path)
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(format!(
                "Path '{}' contains parent traversal in an unresolved path segment",
                path
            ));
        }

        let mut prefix = resolved.clone();
        let mut tail: Vec<std::ffi::OsString> = Vec::new();

        while !prefix.exists() {
            if let Some(name) = prefix.file_name() {
                tail.push(name.to_os_string());
            }
            if !prefix.pop() {
                return Err(format!(
                    "No existing parent directory found for path '{}'",
                    resolved.display()
                ));
            }
        }

        let canonical_prefix = prefix.canonicalize().map_err(|e| {
            format!(
                "Cannot canonicalize parent directory '{}': {}",
                prefix.display(),
                e
            )
        })?;

        let mut full_path = canonical_prefix;
        for comp in tail.into_iter().rev() {
            if comp == OsStr::new("..") {
                return Err(format!(
                    "Path '{}' contains parent traversal in an unresolved path segment",
                    path
                ));
            }
            full_path.push(comp);
        }

        let normalized = normalize_lexical(&full_path)?;

        if !normalized.starts_with(&canonical_cwd) {
            return Err(format!(
                "Path '{}' resolves outside the workspace root '{}'",
                path,
                canonical_cwd.display()
            ));
        }

        Ok(normalized)
    }
}

fn normalize_lexical(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(format!(
                        "Path '{}' contains parent traversal above filesystem root",
                        path.display()
                    ));
                }
            }
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_workspace_path_rejects_unresolved_parent_escape() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_workspace_path("new/../../outside.txt", dir.path())
            .expect_err("unresolved parent traversal must not escape workspace");

        assert!(
            err.contains("outside the workspace") || err.contains("parent traversal"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_workspace_path_allows_new_child_path() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_workspace_path("new/file.txt", dir.path()).unwrap();

        assert!(resolved.starts_with(dir.path().canonicalize().unwrap()));
        assert!(resolved.ends_with("new/file.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_workspace_path_rejects_new_child_under_external_symlink() {
        let workspace = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(external.path(), workspace.path().join("outside-link")).unwrap();

        let err = resolve_workspace_path("outside-link/new-file.txt", workspace.path())
            .expect_err("new files below external symlink must stay blocked");

        assert!(
            err.contains("outside the workspace"),
            "unexpected error: {err}"
        );
    }
}
