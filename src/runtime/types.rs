use crate::core::{RiskClass, ToolResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouterIntent {
    Explore,
    Plan,
    Implement,
    Verify,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyLevel {
    Conservative,
    Balanced,
    Aggressive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub id: String,
    pub tool_name: String,
    pub summary: String,
    pub risk: RiskClass,
    pub status: ApprovalStatus,
    pub created_at: DateTime<Utc>,
    pub tool_arguments: Option<String>,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundJobStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackgroundJob {
    pub id: String,
    pub title: String,
    pub status: BackgroundJobStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkspacePreflight {
    pub branch: String,
    pub dirty_files: Vec<String>,
    pub recent_summary: Option<String>,
    pub suggested_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DiagnosticSummary {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LspServerSnapshot {
    pub language: String,
    pub command: String,
    pub ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SymbolJump {
    pub name: String,
    pub file_path: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LspSnapshot {
    pub ready: bool,
    pub active_roots: Vec<String>,
    pub diagnostics: Vec<DiagnosticSummary>,
    pub symbol_provider: String,
    pub servers: Vec<LspServerSnapshot>,
    pub symbol_jumps: Vec<SymbolJump>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpServerStatus {
    Connected,
    Disconnected,
    Degraded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerSnapshot {
    pub name: String,
    pub status: McpServerStatus,
    pub tool_count: usize,
    pub approval_mode: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct McpSnapshot {
    pub ready: bool,
    pub servers: Vec<McpServerSnapshot>,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ComposerState {
    pub input: String,
    pub context_items: Vec<String>,
    pub slash_suggestions: Vec<String>,
    pub active_target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifecycle {
    Started,
    Resumed,
    Idle,
    WaitingForApproval,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolExecution {
    pub tool_name: String,
    pub summary: String,
    pub result_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    SessionLifecycle {
        session_id: String,
        lifecycle: SessionLifecycle,
        summary: String,
    },
    MessageDelta {
        role: String,
        content: String,
    },
    StreamDelta {
        role: String,
        content: String,
        model: Option<String>,
    },
    StreamDone {
        model: Option<String>,
    },
    RouterStateChanged {
        intent: RouterIntent,
        source: String,
    },
    ToolCallStarted {
        execution: ToolExecution,
    },
    ToolCallFinished {
        execution: ToolExecution,
        result: ToolResult,
    },
    ApprovalRequested {
        approval: ApprovalRequest,
    },
    ApprovalResolved {
        approval: ApprovalRequest,
    },
    DiagnosticsUpdated {
        lsp: LspSnapshot,
    },
    McpStateUpdated {
        mcp: McpSnapshot,
    },
    BackgroundJobUpdated {
        job: BackgroundJob,
    },
    PreflightReady {
        preflight: WorkspacePreflight,
    },
}
