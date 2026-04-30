use crate::core::{ToolResult, resolve_workspace_path};
use serde_json::Value;
use std::path::Path;
use tokio::fs;

#[allow(dead_code)]
pub async fn read_range(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let file_path = args["file_path"].as_str().unwrap_or("");
    let offset = args["offset"].as_u64().map(|v| v as usize).unwrap_or(1);
    let limit = args["limit"].as_u64().map(|v| v as usize);

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

    let content = match fs::read_to_string(&resolved).await {
        Ok(c) => c,
        Err(e) => {
            return Ok(read_error(file_path, &resolved, e));
        }
    };

    Ok(render_read_range(
        file_path, &resolved, &content, offset, limit, None,
    ))
}

pub async fn read_range_with_cache(
    args: Value,
    cwd: &Path,
    cache: &mut super::fast_executor::FileCache,
) -> anyhow::Result<ToolResult> {
    let file_path = args["file_path"].as_str().unwrap_or("");
    let offset = args["offset"].as_u64().map(|v| v as usize).unwrap_or(1);
    let limit = args["limit"].as_u64().map(|v| v as usize);

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

    let read = match cache.read(&resolved).await {
        Ok(read) => read,
        Err(e) => {
            return Ok(read_error(file_path, &resolved, e));
        }
    };

    Ok(render_read_range(
        file_path,
        &resolved,
        &read.content,
        offset,
        limit,
        Some(read.cache_hit),
    ))
}

fn render_read_range(
    file_path: &str,
    resolved: &Path,
    content: &str,
    offset: usize,
    limit: Option<usize>,
    cache_hit: Option<bool>,
) -> ToolResult {
    let lines: Vec<&str> = content.lines().collect();
    let start = offset.saturating_sub(1);
    let end = limit
        .map(|l| (start + l).min(lines.len()))
        .unwrap_or(lines.len());
    let slice = &lines[start..end];

    let numbered: Vec<String> = slice
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{}: {}", start + i + 1, line))
        .collect();

    ToolResult {
        success: true,
        output: numbered.join("\n"),
        error: None,
        metadata: Some(serde_json::json!({
            "file_path": file_path,
            "resolved_path": resolved.display().to_string(),
            "total_lines": lines.len(),
            "offset": offset,
            "limit": limit.unwrap_or(lines.len()),
            "bytes_read": numbered.join("\n").len(),
            "cache_hit": cache_hit,
        })),
    }
}

fn read_error(file_path: &str, resolved: &Path, error: impl std::fmt::Display) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(format!("{}", error)),
        metadata: Some(serde_json::json!({
            "file_path": file_path,
            "resolved_path": resolved.display().to_string()
        })),
    }
}

pub async fn write_file(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let file_path = args["file_path"].as_str().unwrap_or("");
    let content = args["content"].as_str().unwrap_or("");

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

    if let Some(parent) = resolved.parent() {
        if let Err(e) = fs::create_dir_all(parent).await {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("{}", e)),
                metadata: Some(serde_json::json!({
                    "file_path": file_path,
                    "resolved_path": resolved.display().to_string()
                })),
            });
        }
    }

    if let Err(e) = fs::write(&resolved, content).await {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("{}", e)),
            metadata: Some(serde_json::json!({
                "file_path": file_path,
                "resolved_path": resolved.display().to_string()
            })),
        });
    }

    Ok(ToolResult {
        success: true,
        output: format!("Wrote {}", resolved.display()),
        error: None,
        metadata: Some(serde_json::json!({
            "file_path": file_path,
            "resolved_path": resolved.display().to_string(),
            "edit_type": "write",
            "bytes_written": content.len()
        })),
    })
}
