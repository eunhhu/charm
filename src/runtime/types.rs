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
    Yolo,
}

impl AutonomyLevel {
    pub fn label(&self) -> &'static str {
        match self {
            AutonomyLevel::Conservative => "Conservative",
            AutonomyLevel::Balanced => "Balanced",
            AutonomyLevel::Aggressive => "Aggressive",
            AutonomyLevel::Yolo => "YOLO",
        }
    }

    pub fn short(&self) -> &'static str {
        match self {
            AutonomyLevel::Conservative => "safe",
            AutonomyLevel::Balanced => "balanced",
            AutonomyLevel::Aggressive => "fast",
            AutonomyLevel::Yolo => "yolo",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "conservative" | "safe" | "strict" => Some(AutonomyLevel::Conservative),
            "balanced" | "default" | "normal" => Some(AutonomyLevel::Balanced),
            "aggressive" | "auto" | "aggr" => Some(AutonomyLevel::Aggressive),
            "yolo" | "wild" | "auto-all" | "yeet" => Some(AutonomyLevel::Yolo),
            _ => None,
        }
    }

    pub fn detail(&self) -> &'static str {
        match self {
            AutonomyLevel::Conservative => "every write/exec waits for approval",
            AutonomyLevel::Balanced => "reads + safe exec auto, stateful work asks",
            AutonomyLevel::Aggressive => "edits & tests auto, destructive asks",
            AutonomyLevel::Yolo => "everything auto-approved — careful with destructive ops",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            AutonomyLevel::Conservative => AutonomyLevel::Balanced,
            AutonomyLevel::Balanced => AutonomyLevel::Aggressive,
            AutonomyLevel::Aggressive => AutonomyLevel::Yolo,
            AutonomyLevel::Yolo => AutonomyLevel::Conservative,
        }
    }
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
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundJobKind {
    #[default]
    Command,
    SubAgent,
    Verification,
    Index,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackgroundJob {
    pub id: String,
    pub title: String,
    pub status: BackgroundJobStatus,
    pub detail: String,
    #[serde(default)]
    pub kind: BackgroundJobKind,
    #[serde(default)]
    pub progress: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct VerificationState {
    pub required: Vec<String>,
    pub observed: Vec<String>,
    pub satisfied: bool,
    pub last_status: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
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
    Modal {
        title: String,
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
    AutonomyChanged {
        autonomy: AutonomyLevel,
        source: String,
    },
    ModelChanged {
        model: String,
        display: String,
    },
    ContextCompacted {
        removed_messages: usize,
        summary: String,
    },
    SessionSwitched {
        session_id: String,
        title: String,
    },
    SubAgentSpawned {
        job_id: String,
        title: String,
    },
    UsageUpdated {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
    },
}
