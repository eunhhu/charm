use crate::core::ToolResult;
use anyhow::Context;
use serde_json::Value;
use std::path::Path;
use std::process::Stdio;

async fn run_gh_command(args: &[&str], cwd: &Path) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("gh")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to run 'gh' command. Is GitHub CLI installed?")?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(anyhow::anyhow!("gh command failed: {}", stderr))
    }
}

pub async fn github_pr_list(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);

    let state = args.get("state").and_then(|v| v.as_str()).unwrap_or("open");

    let output = run_gh_command(
        &[
            "pr",
            "list",
            "-L",
            &limit.to_string(),
            "--state",
            state,
            "--json",
            "number,title,author,state",
        ],
        cwd,
    )
    .await?;

    Ok(ToolResult {
        success: true,
        output,
        error: None,
        metadata: Some(serde_json::json!({
            "state": state,
            "limit": limit
        })),
    })
}

pub async fn github_pr_view(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let pr_number = args
        .get("pr_number")
        .and_then(|v| v.as_u64())
        .context("Missing 'pr_number' parameter")?;

    let output = run_gh_command(
        &[
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "number,title,body,state,author,files",
        ],
        cwd,
    )
    .await?;

    Ok(ToolResult {
        success: true,
        output,
        error: None,
        metadata: Some(serde_json::json!({
            "pr_number": pr_number
        })),
    })
}

pub async fn github_issue_list(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);

    let state = args.get("state").and_then(|v| v.as_str()).unwrap_or("open");

    let output = run_gh_command(
        &[
            "issue",
            "list",
            "-L",
            &limit.to_string(),
            "--state",
            state,
            "--json",
            "number,title,state,author",
        ],
        cwd,
    )
    .await?;

    Ok(ToolResult {
        success: true,
        output,
        error: None,
        metadata: Some(serde_json::json!({
            "state": state,
            "limit": limit
        })),
    })
}

pub async fn github_issue_view(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let issue_number = args
        .get("issue_number")
        .and_then(|v| v.as_u64())
        .context("Missing 'issue_number' parameter")?;

    let output = run_gh_command(
        &[
            "issue",
            "view",
            &issue_number.to_string(),
            "--json",
            "number,title,body,state,author,comments",
        ],
        cwd,
    )
    .await?;

    Ok(ToolResult {
        success: true,
        output,
        error: None,
        metadata: Some(serde_json::json!({
            "issue_number": issue_number
        })),
    })
}

pub async fn github_repo_view(_args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let output = run_gh_command(
        &[
            "repo",
            "view",
            "--json",
            "name,description,url,defaultBranch",
        ],
        cwd,
    )
    .await?;

    Ok(ToolResult {
        success: true,
        output,
        error: None,
        metadata: None,
    })
}
