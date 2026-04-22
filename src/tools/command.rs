use crate::core::ToolResult;
use serde_json::Value;
use std::collections::HashMap;
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

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&effective_command)
        .current_dir(&cwd)
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;

    let cid = command_id.clone();
    tokio::spawn(async move {
        let status = child.wait().await;
        let mut store = COMMAND_STORE.lock().unwrap();
        if let Some(entry) = store.get_mut(&cid) {
            entry.running = false;
            entry.exit_code = status.ok().and_then(|s| s.code());
        }
    });

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
