use crate::core::ToolResult;
use crate::harness::MemoryManager;
use serde_json::Value;

pub async fn memory_stage(args: Value, cwd: &std::path::Path) -> anyhow::Result<ToolResult> {
    let scope = args["scope"].as_str().unwrap_or("session");
    let category = args["category"].as_str().unwrap_or("general");
    let content = args["content"].as_str().unwrap_or("");

    if content.is_empty() {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("content cannot be empty".to_string()),
            metadata: None,
        });
    }

    let mut manager = MemoryManager::new(cwd);
    let id = manager.stage(scope, category, content);
    manager.save()?;

    Ok(ToolResult {
        success: true,
        output: format!(
            "Staged memory {} (scope: {}, approved: {})",
            id,
            scope,
            scope == "session"
        ),
        error: None,
        metadata: Some(serde_json::json!({"memory_id": id, "scope": scope})),
    })
}

pub async fn memory_commit(args: Value, cwd: &std::path::Path) -> anyhow::Result<ToolResult> {
    let ids: Vec<String> = args["memory_ids"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    if ids.is_empty() {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("memory_ids cannot be empty".to_string()),
            metadata: None,
        });
    }

    let mut manager = MemoryManager::new(cwd);
    let count = manager.commit(&ids);
    manager.save()?;

    Ok(ToolResult {
        success: true,
        output: format!("Committed {} memories", count),
        error: None,
        metadata: Some(serde_json::json!({"committed": count})),
    })
}
