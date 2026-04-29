use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReasoningConfig {
    pub effort: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallBlock {
    pub id: String,
    pub r#type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolSchema {
    pub r#type: String,
    pub function: FunctionSchema,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionSchema {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    pub message: Message,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(default)]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: u32,
}

pub fn default_tool_schemas() -> Vec<ToolSchema> {
    vec![
        tool(
            "read_range",
            "Read a range of lines from a file",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 1 },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["file_path"]
            }),
        ),
        tool(
            "grep_search",
            "Search for a pattern using ripgrep",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "include": { "type": "string" },
                    "output_mode": { "type": "string", "enum": ["content", "files_with_matches", "count"] }
                },
                "required": ["pattern"]
            }),
        ),
        tool(
            "glob_search",
            "Find files matching a glob pattern",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" }
                },
                "required": ["pattern"]
            }),
        ),
        tool(
            "semantic_search",
            "Search the pre-built AST symbol index for functions, classes, and methods matching a query",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "top_k": { "type": "integer", "minimum": 1, "maximum": 50 },
                    "expand_full": { "type": "boolean" }
                },
                "required": ["query"]
            }),
        ),
        tool(
            "parallel_search",
            "Run parallel grep + semantic search and return ranked, deduplicated evidence. Use this when you need to find the right file quickly.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "top_k": { "type": "integer", "minimum": 1, "maximum": 30 }
                },
                "required": ["query"]
            }),
        ),
        tool(
            "list_dir",
            "List directory contents",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "dir_path": { "type": "string" }
                },
                "required": ["dir_path"]
            }),
        ),
        tool(
            "edit_patch",
            "Apply a targeted patch edit to a file",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" }
                },
                "required": ["file_path", "old_string", "new_string"]
            }),
        ),
        tool(
            "write_file",
            "Write content to a file",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["file_path", "content"]
            }),
        ),
        tool(
            "run_command",
            "Run a shell command",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "cwd": { "type": "string" },
                    "blocking": { "type": "boolean" },
                    "timeout_ms": { "type": "integer" },
                    "risk_class": { "type": "string", "enum": ["safe-read", "safe-exec", "stateful-exec", "destructive", "external-side-effect"] }
                },
                "required": ["command"]
            }),
        ),
        tool(
            "poll_command",
            "Poll a previously started non-blocking command",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command_id": { "type": "string" },
                    "output_priority": { "type": "string", "enum": ["top", "bottom", "split"] },
                    "max_lines": { "type": "integer", "minimum": 1 }
                },
                "required": ["command_id"]
            }),
        ),
        tool(
            "cancel_command",
            "Cancel a previously started non-blocking command",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command_id": { "type": "string" }
                },
                "required": ["command_id"]
            }),
        ),
        tool(
            "checkpoint_create",
            "Create a git checkpoint before risky operations",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "scope": { "type": "string", "enum": ["auto", "phase", "manual"] }
                },
                "required": ["name"]
            }),
        ),
        tool(
            "checkpoint_restore",
            "Restore a previously created checkpoint",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "checkpoint_id": { "type": "string" }
                },
                "required": ["checkpoint_id"]
            }),
        ),
        tool(
            "plan_update",
            "Update the plan.md artifact",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "objective": { "type": "string" },
                    "current_phase": { "type": "string" },
                    "completed_steps": { "type": "array", "items": { "type": "string" } },
                    "blocked_steps": { "type": "array", "items": { "type": "string" } },
                    "notes": { "type": "string" }
                },
                "required": []
            }),
        ),
        tool(
            "memory_stage",
            "Stage a memory entry. Session memories are auto-approved. Project/user memories require commit.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "enum": ["session", "project", "user"] },
                    "category": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["scope", "category", "content"]
            }),
        ),
        tool(
            "memory_commit",
            "Commit staged project/user memories to durable storage",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "memory_ids": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["memory_ids"]
            }),
        ),
    ]
}

fn tool(name: &str, description: &str, parameters: Value) -> ToolSchema {
    ToolSchema {
        r#type: "function".to_string(),
        function: FunctionSchema {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
        },
    }
}
