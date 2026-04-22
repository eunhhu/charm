use crate::core::ToolResult;
use serde_json::Value;
use tokio::fs;

pub async fn edit_patch(args: Value) -> anyhow::Result<ToolResult> {
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

    let content = fs::read_to_string(file_path)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", file_path, e))?;

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
    fs::write(file_path, updated)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", file_path, e))?;

    Ok(ToolResult {
        success: true,
        output: format!("Patched {}", file_path),
        error: None,
        metadata: Some(serde_json::json!({
            "file_path": file_path,
            "edit_type": "patch",
            "bytes_written": new_string.len()
        })),
    })
}
