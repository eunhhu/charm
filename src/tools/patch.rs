use crate::core::{ToolResult, resolve_workspace_path};
use serde_json::Value;
use std::path::Path;
use tokio::fs;

pub async fn edit_patch(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let file_path = args["file_path"].as_str().unwrap_or("");
    let old_string = args["old_string"].as_str().unwrap_or("");
    let new_string = args["new_string"].as_str().unwrap_or("");

    if old_string.is_empty() {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("old_string cannot be empty".to_string()),
            metadata: None,
        });
    }

    let resolved = match resolve_workspace_path(file_path, cwd) {
        Ok(p) => p,
        Err(e) => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e),
                metadata: Some(serde_json::json!({
                    "file_path": file_path,
                })),
            });
        }
    };

    let content = fs::read_to_string(&resolved)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", resolved.display(), e))?;

    if !content.contains(old_string) {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("old_string not found in {}", file_path)),
            metadata: None,
        });
    }

    let count = content.matches(old_string).count();
    if count > 1 {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!(
                "old_string found {} times in {} — ambiguous",
                count, file_path
            )),
            metadata: None,
        });
    }

    let updated = content.replacen(old_string, new_string, 1);
    fs::write(&resolved, updated)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", resolved.display(), e))?;

    Ok(ToolResult {
        success: true,
        output: format!("Patched {}", resolved.display()),
        error: None,
        metadata: Some(serde_json::json!({
            "file_path": file_path,
            "resolved_path": resolved.display().to_string(),
            "edit_type": "patch",
            "bytes_written": new_string.len()
        })),
    })
}
