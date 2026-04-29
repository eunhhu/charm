use crate::core::{ToolCall, ToolResult};
use crate::tools::{FastExecutor, ToolRegistry};

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
        let remaining = self.remaining_budget();
        if remaining == 0 {
            return vec![ToolResult {
                success: false,
                output: String::new(),
                error: Some("Tool budget exhausted".to_string()),
                metadata: None,
            }];
        }

        let overflow = calls.len().saturating_sub(remaining);
        let allowed: Vec<ToolCall> = calls.into_iter().take(remaining).collect();
        self.tool_count += allowed.len();

        let mut results = match FastExecutor::execute_batch(allowed, &mut self.registry).await {
            Ok(results) => results,
            Err(e) => vec![ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
                metadata: None,
            }],
        };

        for _ in 0..overflow {
            results.push(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Tool budget exhausted".to_string()),
                metadata: None,
            });
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
