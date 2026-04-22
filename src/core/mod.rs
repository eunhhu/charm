use serde::{Deserialize, Serialize};

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
