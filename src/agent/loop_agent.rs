use crate::core::{ToolCall, ToolResult};
use crate::tools::ToolRegistry;

pub struct AgentLoop {
    registry: ToolRegistry,
    tool_budget: usize,
    tool_count: usize,
}

impl AgentLoop {
    pub fn new(registry: ToolRegistry) -> Self {
        Self {
            registry,
            tool_budget: 20,
            tool_count: 0,
        }
    }

    pub async fn run_tool_calls(&mut self, calls: Vec<ToolCall>) -> Vec<ToolResult> {
        let mut results = Vec::new();

        for call in calls {
            if self.tool_count >= self.tool_budget {
                results.push(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Tool budget exhausted".to_string()),
                    metadata: None,
                });
                break;
            }
            self.tool_count += 1;

            let tool_name = match &call {
                ToolCall::ReadRange { .. } => "read_range",
                ToolCall::ReadSymbol { .. } => "read_symbol",
                ToolCall::GrepSearch { .. } => "grep_search",
                ToolCall::GlobSearch { .. } => "glob_search",
                ToolCall::ListDir { .. } => "list_dir",
                ToolCall::SemanticSearch { .. } => "semantic_search",
                ToolCall::ParallelSearch { .. } => "parallel_search",
                ToolCall::EditPatch { .. } => "edit_patch",
                ToolCall::WriteFile { .. } => "write_file",
                ToolCall::RunCommand { .. } => "run_command",
                ToolCall::PollCommand { .. } => "poll_command",
                ToolCall::PlanUpdate { .. } => "plan_update",
                ToolCall::CheckpointCreate { .. } => "checkpoint_create",
                ToolCall::CheckpointRestore { .. } => "checkpoint_restore",
                ToolCall::MemoryStage { .. } => "memory_stage",
                ToolCall::MemoryCommit { .. } => "memory_commit",
            };

            let args = serde_json::to_value(&call).unwrap_or_default();
            match self.registry.execute(tool_name, args).await {
                Ok(result) => results.push(result),
                Err(e) => results.push(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                    metadata: None,
                }),
            }
        }

        results
    }

    pub fn remaining_budget(&self) -> usize {
        self.tool_budget - self.tool_count
    }

    pub fn reset_budget(&mut self) {
        self.tool_count = 0;
    }
}
