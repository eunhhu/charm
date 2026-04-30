use crate::core::{ToolResult, resolve_workspace_path};
use serde_json::Value;
use std::io::ErrorKind;
use std::path::Path;
use tokio::process::Command;

pub async fn grep_search(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    grep_search_with_binary(args, cwd, "rg").await
}

async fn grep_search_with_binary(
    args: Value,
    cwd: &Path,
    rg_binary: &str,
) -> anyhow::Result<ToolResult> {
    let pattern = args["pattern"].as_str().unwrap_or("");
    let path_arg = args["path"].as_str().unwrap_or(".");
    let include = args["include"].as_str();
    let output_mode = args["output_mode"].as_str().unwrap_or("content");

    let resolved = match resolve_workspace_path(path_arg, cwd) {
        Ok(p) => p,
        Err(e) => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e),
                metadata: Some(serde_json::json!({
                    "search_path": path_arg,
                })),
            });
        }
    };

    let mut cmd = Command::new(rg_binary);
    cmd.arg("-n")
        .arg("--color=never")
        .arg("-F")
        .arg(pattern)
        .arg(&resolved);
    if let Some(inc) = include {
        cmd.arg("-g").arg(inc);
    }

    let output = match cmd.output().await {
        Ok(output) => output,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            let lines = fallback_grep_lines(pattern, &resolved, include)?;
            return Ok(grep_result_from_lines(
                lines,
                output_mode,
                path_arg,
                &resolved,
            ));
        }
        Err(err) => return Err(err.into()),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);

    if !output.status.success() && stdout.is_empty() {
        return Ok(ToolResult {
            success: true,
            output: "(no matches)".to_string(),
            error: None,
            metadata: Some(serde_json::json!({
                "search_path": path_arg,
                "resolved_path": resolved.display().to_string(),
                "match_count": 0,
                "matched_files": []
            })),
        });
    }

    let lines: Vec<String> = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    Ok(grep_result_from_lines(
        lines,
        output_mode,
        path_arg,
        &resolved,
    ))
}

fn grep_result_from_lines(
    lines: Vec<String>,
    output_mode: &str,
    path_arg: &str,
    resolved: &Path,
) -> ToolResult {
    if lines.is_empty() {
        return ToolResult {
            success: true,
            output: "(no matches)".to_string(),
            error: None,
            metadata: Some(serde_json::json!({
                "search_path": path_arg,
                "resolved_path": resolved.display().to_string(),
                "match_count": 0,
                "matched_files": []
            })),
        };
    }

    let mut matched_files: Vec<String> = lines
        .iter()
        .filter_map(|line| line.split(':').next())
        .map(|value| value.to_string())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    matched_files.sort();

    let result = match output_mode {
        "files_with_matches" => matched_files.join("\n"),
        "count" => lines.len().to_string(),
        _ => lines.join("\n"),
    };

    ToolResult {
        success: true,
        output: result,
        error: None,
        metadata: Some(serde_json::json!({
            "search_path": path_arg,
            "resolved_path": resolved.display().to_string(),
            "match_count": lines.len(),
            "matched_files": matched_files
        })),
    }
}

fn fallback_grep_lines(
    pattern: &str,
    resolved: &Path,
    include: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let mut lines = Vec::new();
    for entry in walkdir::WalkDir::new(resolved)
        .into_iter()
        .filter_entry(|entry| !is_skipped_search_entry(entry.path(), resolved))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !include_allows_path(path, resolved, include) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for (index, line) in content.lines().enumerate() {
            if line.contains(pattern) {
                lines.push(format!("{}:{}:{}", path.display(), index + 1, line));
            }
        }
    }
    Ok(lines)
}

fn is_skipped_search_entry(path: &Path, root: &Path) -> bool {
    if path == root {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with('.') || matches!(name, "target" | "node_modules")
}

fn include_allows_path(path: &Path, root: &Path, include: Option<&str>) -> bool {
    let Some(include) = include else {
        return true;
    };
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    glob_match(include, file_name) || glob_match(include, &rel)
}

pub async fn glob_search(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let pattern = args["pattern"].as_str().unwrap_or("*");
    let path_arg = args["path"].as_str().unwrap_or(".");

    let resolved = match resolve_workspace_path(path_arg, cwd) {
        Ok(p) => p,
        Err(e) => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e),
                metadata: Some(serde_json::json!({
                    "search_path": path_arg,
                })),
            });
        }
    };

    let mut results = Vec::new();
    for entry in walkdir::WalkDir::new(&resolved)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            let name = entry.file_name().to_string_lossy();
            if glob_match(pattern, &name) {
                results.push(entry.path().to_string_lossy().to_string());
            }
        }
    }

    Ok(ToolResult {
        success: true,
        output: results.join("\n"),
        error: None,
        metadata: Some(serde_json::json!({
            "search_path": path_arg,
            "resolved_path": resolved.display().to_string(),
            "match_count": results.len(),
            "matched_files": results
        })),
    })
}

pub async fn list_dir(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let dir_path = args["dir_path"].as_str().unwrap_or(".");

    let resolved = match resolve_workspace_path(dir_path, cwd) {
        Ok(p) => p,
        Err(e) => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e),
                metadata: Some(serde_json::json!({
                    "search_path": dir_path,
                })),
            });
        }
    };

    let mut entries = Vec::new();

    let mut dir = tokio::fs::read_dir(&resolved).await?;
    while let Some(entry) = dir.next_entry().await? {
        let meta = entry.metadata().await?;
        let name = entry.file_name().to_string_lossy().to_string();
        let kind = if meta.is_dir() { "d" } else { "f" };
        entries.push(format!("{} {}", kind, name));
    }

    Ok(ToolResult {
        success: true,
        output: entries.join("\n"),
        error: None,
        metadata: Some(serde_json::json!({
            "search_path": dir_path,
            "resolved_path": resolved.display().to_string(),
            "match_count": 0,
            "matched_files": []
        })),
    })
}

fn glob_match(pattern: &str, name: &str) -> bool {
    let regex = pattern
        .replace(".", "\\.")
        .replace("*", ".*")
        .replace("?", ".");
    regex::Regex::new(&format!("^{}$", regex))
        .map(|re| re.is_match(name))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn grep_search_falls_back_when_rg_binary_is_missing() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src").join("main.rs"),
            "fn panic_path() { panic!(\"boom\"); }\n",
        )
        .unwrap();

        let result = grep_search_with_binary(
            serde_json::json!({
                "pattern": "panic_path",
                "output_mode": "content"
            }),
            dir.path(),
            "__missing_rg_for_test__",
        )
        .await
        .unwrap();

        assert!(result.success);
        assert!(result.output.contains("src/main.rs:1:fn panic_path"));
        assert_eq!(
            result
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("match_count"))
                .and_then(Value::as_u64),
            Some(1)
        );
    }

    #[tokio::test]
    async fn grep_search_fallback_skips_hidden_session_state_like_rg() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".charm").join("sessions")).unwrap();
        std::fs::write(
            dir.path()
                .join(".charm")
                .join("sessions")
                .join("messages.json"),
            "write_file delete scope guard",
        )
        .unwrap();

        let result = grep_search_with_binary(
            serde_json::json!({
                "pattern": "write_file",
                "output_mode": "content"
            }),
            dir.path(),
            "__missing_rg_for_test__",
        )
        .await
        .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "(no matches)");
        assert_eq!(
            result
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("match_count"))
                .and_then(Value::as_u64),
            Some(0)
        );
    }
}
