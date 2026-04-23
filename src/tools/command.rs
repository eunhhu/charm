use crate::core::ToolResult;
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;
use tokio::process::Command;
use uuid::Uuid;

lazy_static::lazy_static! {
    static ref COMMAND_STORE: Mutex<HashMap<String, CommandEntry>> = Mutex::new(HashMap::new());
}

struct CommandEntry {
    running: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

pub async fn run_command(args: Value, default_cwd: &std::path::Path) -> anyhow::Result<ToolResult> {
    let command_str = args["command"].as_str().unwrap_or("");
    let cwd = args["cwd"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| default_cwd.to_string_lossy().to_string());
    let blocking = args["blocking"].as_bool().unwrap_or(true);
    let timeout_ms = args["timeout_ms"].as_u64();

    let command_id = Uuid::new_v4().to_string();

    let effective_command = if blocking {
        crate::tools::rtk_filter::rewrite_with_rtk(command_str)
            .unwrap_or_else(|| command_str.to_string())
    } else {
        command_str.to_string()
    };

    if blocking {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(&effective_command)
            .current_dir(&cwd)
            .kill_on_drop(true);

        let output = if let Some(ms) = timeout_ms {
            tokio::time::timeout(tokio::time::Duration::from_millis(ms), cmd.output()).await??
        } else {
            cmd.output().await?
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let success = output.status.success();

        return Ok(ToolResult {
            success,
            output: if stderr.is_empty() {
                stdout.clone()
            } else {
                format!("{}\n---stderr---\n{}", stdout, stderr)
            },
            error: if success { None } else { Some(stderr) },
            metadata: Some(serde_json::json!({
                "command_id": command_id,
                "command": command_str,
                "executed": effective_command,
                "cwd": cwd,
                "exit_code": output.status.code()
            })),
        });
    }

    // Insert entry FIRST to avoid race condition with background task
    {
        let mut store = COMMAND_STORE.lock().unwrap();
        store.insert(
            command_id.clone(),
            CommandEntry {
                running: true,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
            },
        );
    }

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&effective_command)
        .current_dir(&cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn()?;

    let cid = command_id.clone();
    tokio::spawn(async move {
        let output = child.wait_with_output().await;
        let mut store = COMMAND_STORE.lock().unwrap();
        if let Some(entry) = store.get_mut(&cid) {
            entry.running = false;
            if let Ok(out) = output {
                entry.exit_code = out.status.code();
                entry.stdout = String::from_utf8_lossy(&out.stdout).to_string();
                entry.stderr = String::from_utf8_lossy(&out.stderr).to_string();
            }
        }
    });

    Ok(ToolResult {
        success: true,
        output: format!("Command started: {}", command_id),
        error: None,
        metadata: Some(serde_json::json!({
            "command_id": command_id,
            "command": command_str,
            "executed": effective_command,
            "cwd": cwd,
            "running": true
        })),
    })
}

pub async fn poll_command(args: Value) -> anyhow::Result<ToolResult> {
    let command_id = args["command_id"].as_str().unwrap_or("");
    let output_priority = args["output_priority"].as_str().unwrap_or("bottom");
    let max_lines = args["max_lines"].as_u64().map(|v| v as usize);

    let store = COMMAND_STORE.lock().unwrap();
    let entry = store
        .get(command_id)
        .ok_or_else(|| anyhow::anyhow!("Command not found: {}", command_id))?;

    let combined = if entry.stderr.is_empty() {
        entry.stdout.clone()
    } else {
        format!("{}\n---stderr---\n{}", entry.stdout, entry.stderr)
    };

    let output = if let Some(max) = max_lines {
        let lines: Vec<&str> = combined.lines().collect();
        if lines.len() > max {
            match output_priority {
                "top" => lines[..max].join("\n"),
                "bottom" => lines[lines.len().saturating_sub(max)..].join("\n"),
                _ => {
                    let half = max / 2;
                    format!(
                        "{}\n...\n{}",
                        lines[..half].join("\n"),
                        lines[lines.len().saturating_sub(half)..].join("\n")
                    )
                }
            }
        } else {
            combined
        }
    } else {
        combined
    };

    Ok(ToolResult {
        success: true,
        output,
        error: None,
        metadata: Some(
            serde_json::json!({ "running": entry.running, "exit_code": entry.exit_code }),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn nonblocking_command_starts_in_running_state() {
        let args = serde_json::json!({
            "command": "sleep 5",
            "blocking": false
        });
        let cwd = std::env::current_dir().unwrap();

        let result = run_command(args, &cwd).await.unwrap();
        assert!(result.success);

        let meta = result.metadata.unwrap();
        let command_id = meta["command_id"].as_str().unwrap();

        // Poll immediately - should be running
        let poll_args = serde_json::json!({"command_id": command_id});
        let poll_result = poll_command(poll_args).await.unwrap();

        let poll_meta = poll_result.metadata.unwrap();
        assert!(poll_meta["running"].as_bool().unwrap());
        assert!(poll_meta["exit_code"].is_null());
    }

    #[tokio::test]
    async fn nonblocking_command_captures_stdout_after_completion() {
        let args = serde_json::json!({
            "command": "echo 'hello world'",
            "blocking": false
        });
        let cwd = std::env::current_dir().unwrap();

        let result = run_command(args, &cwd).await.unwrap();
        let command_id = result.metadata.unwrap()["command_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Wait for command to complete
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Poll after completion
        let poll_args = serde_json::json!({"command_id": command_id});
        let poll_result = poll_command(poll_args).await.unwrap();

        let poll_meta = poll_result.metadata.unwrap();
        assert!(!poll_meta["running"].as_bool().unwrap());
        assert_eq!(poll_meta["exit_code"].as_i64(), Some(0));
        assert!(poll_result.output.contains("hello world"));
    }

    #[tokio::test]
    async fn nonblocking_command_captures_stderr() {
        let args = serde_json::json!({
            "command": "echo 'error msg' >&2",
            "blocking": false
        });
        let cwd = std::env::current_dir().unwrap();

        let result = run_command(args, &cwd).await.unwrap();
        let command_id = result.metadata.unwrap()["command_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Wait for command to complete
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Poll after completion
        let poll_args = serde_json::json!({"command_id": command_id});
        let poll_result = poll_command(poll_args).await.unwrap();

        assert!(poll_result.output.contains("error msg"));
        assert!(poll_result.output.contains("---stderr---"));
    }

    #[tokio::test]
    async fn nonblocking_command_reports_failed_exit_code() {
        let args = serde_json::json!({
            "command": "exit 42",
            "blocking": false
        });
        let cwd = std::env::current_dir().unwrap();

        let result = run_command(args, &cwd).await.unwrap();
        let command_id = result.metadata.unwrap()["command_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Wait for command to complete
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Poll after completion
        let poll_args = serde_json::json!({"command_id": command_id});
        let poll_result = poll_command(poll_args).await.unwrap();

        let poll_meta = poll_result.metadata.unwrap();
        assert!(!poll_meta["running"].as_bool().unwrap());
        assert_eq!(poll_meta["exit_code"].as_i64(), Some(42));
    }

    #[tokio::test]
    async fn blocking_command_works_correctly() {
        let args = serde_json::json!({
            "command": "echo 'blocking test'",
            "blocking": true
        });
        let cwd = std::env::current_dir().unwrap();

        let result = run_command(args, &cwd).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("blocking test"));
        let meta = result.metadata.unwrap();
        assert_eq!(meta["exit_code"].as_i64(), Some(0));
    }

    #[tokio::test]
    async fn poll_command_returns_error_for_invalid_id() {
        let args = serde_json::json!({"command_id": "invalid-id-does-not-exist"});
        let result = poll_command(args).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
