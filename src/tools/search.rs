use crate::core::ToolResult;
use serde_json::Value;
use std::path::Path;
use tokio::process::Command;

pub async fn grep_search(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let pattern = args["pattern"].as_str().unwrap_or("");
    let path = args["path"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| cwd.to_string_lossy().to_string());
    let include = args["include"].as_str();
    let output_mode = args["output_mode"].as_str().unwrap_or("content");

    let mut cmd = Command::new("rg");
    cmd.arg("-n")
        .arg("--color=never")
        .arg("-F")
        .arg(pattern)
        .arg(&path);
    if let Some(inc) = include {
        cmd.arg("-g").arg(inc);
    }

    let output = cmd.output().await?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    if !output.status.success() && stdout.is_empty() {
        return Ok(ToolResult {
            success: true,
            output: "(no matches)".to_string(),
            error: None,
            metadata: Some(serde_json::json!({
                "search_path": path,
                "match_count": 0,
                "matched_files": []
            })),
        });
    }

    let lines: Vec<&str> = stdout.lines().filter(|line| !line.is_empty()).collect();
    let matched_files: Vec<String> = lines
        .iter()
        .filter_map(|line| line.split(':').next())
        .map(|value| value.to_string())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let result = match output_mode {
        "files_with_matches" => matched_files.join("\n"),
        "count" => lines.len().to_string(),
        _ => stdout.to_string(),
    };

    Ok(ToolResult {
        success: true,
        output: result,
        error: None,
        metadata: Some(serde_json::json!({
            "search_path": path,
            "match_count": lines.len(),
            "matched_files": matched_files
        })),
    })
}

pub async fn glob_search(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let pattern = args["pattern"].as_str().unwrap_or("*");
    let path = args["path"]
        .as_str()
        .map(|s| std::path::Path::new(s).to_path_buf())
        .unwrap_or_else(|| cwd.to_path_buf());

    let mut results = Vec::new();
    for entry in walkdir::WalkDir::new(&path)
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
            "search_path": path,
            "match_count": results.len(),
            "matched_files": results
        })),
    })
}

pub async fn list_dir(args: Value) -> anyhow::Result<ToolResult> {
    let dir_path = args["dir_path"].as_str().unwrap_or(".");
    let mut entries = Vec::new();

    let mut dir = tokio::fs::read_dir(dir_path).await?;
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
