use crate::core::{ToolResult, resolve_workspace_path};
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Instant;
use tokio::process::Command;
use uuid::Uuid;

/// Default timeout for blocking commands when no explicit timeout is provided (2 minutes).
const DEFAULT_BLOCKING_TIMEOUT_MS: u64 = 120_000;

/// Wall-time limit for non-blocking commands. After this duration the subprocess
/// is killed and the entry is marked expired so it cannot leak memory forever (10 minutes).
const DEFAULT_NONBLOCKING_WALL_MS: u64 = 600_000;

lazy_static::lazy_static! {
    static ref COMMAND_STORE: Mutex<HashMap<String, CommandEntry>> = Mutex::new(HashMap::new());
}

struct CommandEntry {
    running: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
    cancelled: bool,
    process_id: Option<u32>,
    spawned_at: Instant,
}

pub async fn run_command(args: Value, default_cwd: &std::path::Path) -> anyhow::Result<ToolResult> {
    let command_str = args["command"].as_str().unwrap_or("");
    let cwd = if let Some(cwd_arg) = args["cwd"].as_str() {
        match resolve_workspace_path(cwd_arg, default_cwd) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Invalid cwd: {}", e)),
                    metadata: None,
                });
            }
        }
    } else {
        default_cwd.to_path_buf()
    };
    let cwd_str = cwd.to_string_lossy().to_string();
    let blocking = args["blocking"].as_bool().unwrap_or(true);
    let timeout_ms = args["timeout_ms"].as_u64();

    let command_id = Uuid::new_v4().to_string();

    let effective_command = crate::tools::rtk_filter::rewrite_with_rtk(command_str)
        .unwrap_or_else(|| command_str.to_string());

    if blocking {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(&effective_command)
            .current_dir(&cwd)
            .kill_on_drop(true);

        let effective_timeout_ms = timeout_ms.unwrap_or(DEFAULT_BLOCKING_TIMEOUT_MS);
        let output = match tokio::time::timeout(
            tokio::time::Duration::from_millis(effective_timeout_ms),
            cmd.output(),
        )
        .await
        {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                return Ok(ToolResult {
                    success: false,
                    output: format!("Command timed out after {}ms", effective_timeout_ms),
                    error: Some(format!(
                        "Command timed out after {}ms",
                        effective_timeout_ms
                    )),
                    metadata: Some(serde_json::json!({
                        "command_id": command_id,
                        "command": command_str,
                        "executed": effective_command,
                        "cwd": cwd_str,
                        "timed_out": true,
                        "timeout_ms": effective_timeout_ms,
                    })),
                });
            }
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
                "cwd": cwd_str,
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
                timed_out: false,
                cancelled: false,
                process_id: None,
                spawned_at: Instant::now(),
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
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    let child = cmd.spawn()?;
    {
        let mut store = COMMAND_STORE.lock().unwrap();
        if let Some(entry) = store.get_mut(&command_id) {
            entry.process_id = child.id();
        }
    }

    let cid = command_id.clone();
    let wall_timeout = tokio::time::Duration::from_millis(DEFAULT_NONBLOCKING_WALL_MS);
    tokio::spawn(async move {
        let result = tokio::time::timeout(wall_timeout, child.wait_with_output()).await;
        let mut store = COMMAND_STORE.lock().unwrap();
        if let Some(entry) = store.get_mut(&cid) {
            entry.running = false;
            match result {
                Ok(Ok(out)) => {
                    entry.exit_code = out
                        .status
                        .code()
                        .or(entry.exit_code)
                        .or_else(|| if entry.cancelled { Some(130) } else { None });
                    entry.stdout = String::from_utf8_lossy(&out.stdout).to_string();
                    entry.stderr = String::from_utf8_lossy(&out.stderr).to_string();
                    if entry.cancelled && entry.stderr.is_empty() {
                        entry.stderr.push_str("[command cancelled]");
                    }
                }
                Ok(Err(_)) => {
                    entry.stderr.push_str("\n[subprocess error]");
                }
                Err(_) => {
                    entry.timed_out = true;
                    entry.stderr.push_str(&format!(
                        "\n[wall-time expired after {}ms, process killed]",
                        DEFAULT_NONBLOCKING_WALL_MS
                    ));
                }
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
            "cwd": cwd_str,
            "running": true,
            "wall_timeout_ms": DEFAULT_NONBLOCKING_WALL_MS,
        })),
    })
}

pub async fn poll_command(args: Value) -> anyhow::Result<ToolResult> {
    let command_id = args["command_id"].as_str().unwrap_or("");
    let output_priority = args["output_priority"].as_str().unwrap_or("bottom");
    let max_lines = args["max_lines"].as_u64().map(|v| v as usize);

    let mut store = COMMAND_STORE.lock().unwrap();
    let entry = store
        .get_mut(command_id)
        .ok_or_else(|| anyhow::anyhow!("Command not found: {}", command_id))?;

    // Check wall-time expiry for still-running commands
    if entry.running {
        let elapsed = entry.spawned_at.elapsed();
        if elapsed.as_millis() as u64 > DEFAULT_NONBLOCKING_WALL_MS {
            entry.running = false;
            entry.timed_out = true;
            entry.stderr.push_str(&format!(
                "\n[wall-time expired after {}ms, process killed]",
                DEFAULT_NONBLOCKING_WALL_MS
            ));
        }
    }

    // Evict completed entries older than 30 minutes to prevent unbounded growth
    if !entry.running {
        let age = entry.spawned_at.elapsed();
        if age.as_secs() > 1800 {
            let combined = if entry.stderr.is_empty() {
                entry.stdout.clone()
            } else {
                format!("{}\n---stderr---\n{}", entry.stdout, entry.stderr)
            };
            let exit_code = entry.exit_code;
            let timed_out = entry.timed_out;
            let cancelled = entry.cancelled;
            drop(store);
            COMMAND_STORE.lock().unwrap().remove(command_id);
            return Ok(ToolResult {
                success: false,
                output: combined,
                error: Some("Command entry evicted (completed >30m ago)".to_string()),
                metadata: Some(serde_json::json!({
                    "running": false,
                    "exit_code": exit_code,
                    "timed_out": timed_out,
                    "cancelled": cancelled,
                    "evicted": true,
                })),
            });
        }
    }

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
            serde_json::json!({ "running": entry.running, "exit_code": entry.exit_code, "timed_out": entry.timed_out, "cancelled": entry.cancelled }),
        ),
    })
}

pub async fn cancel_command(args: Value) -> anyhow::Result<ToolResult> {
    let command_id = args["command_id"].as_str().unwrap_or("");
    let process_id = {
        let store = COMMAND_STORE.lock().unwrap();
        let entry = store
            .get(command_id)
            .ok_or_else(|| anyhow::anyhow!("Command not found: {}", command_id))?;
        if !entry.running {
            return Ok(ToolResult {
                success: true,
                output: format!("Command already stopped: {}", command_id),
                error: None,
                metadata: Some(serde_json::json!({
                    "command_id": command_id,
                    "running": false,
                    "exit_code": entry.exit_code,
                    "timed_out": entry.timed_out,
                    "cancelled": entry.cancelled,
                })),
            });
        }
        entry.process_id
    };

    let kill_result = process_id.map(terminate_process).unwrap_or(false);

    let mut store = COMMAND_STORE.lock().unwrap();
    let entry = store
        .get_mut(command_id)
        .ok_or_else(|| anyhow::anyhow!("Command not found: {}", command_id))?;
    entry.running = false;
    entry.cancelled = true;
    if entry.exit_code.is_none() {
        entry.exit_code = Some(130);
    }
    if entry.stderr.is_empty() {
        entry.stderr.push_str("[command cancelled]");
    }

    Ok(ToolResult {
        success: kill_result || process_id.is_none(),
        output: format!("Command cancelled: {}", command_id),
        error: if kill_result || process_id.is_none() {
            None
        } else {
            Some("Failed to signal command process".to_string())
        },
        metadata: Some(serde_json::json!({
            "command_id": command_id,
            "running": false,
            "exit_code": entry.exit_code,
            "timed_out": entry.timed_out,
            "cancelled": true,
            "process_id": process_id,
        })),
    })
}

#[cfg(unix)]
fn terminate_process(pid: u32) -> bool {
    let process_group = format!("-{}", pid);
    signal_term(&process_group) || signal_term(&pid.to_string())
}

#[cfg(not(unix))]
fn terminate_process(pid: u32) -> bool {
    signal_term(&pid.to_string())
}

fn signal_term(target: &str) -> bool {
    std::process::Command::new("kill")
        .arg("-TERM")
        .arg(target)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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

    #[tokio::test]
    async fn blocking_command_times_out_with_default() {
        let args = serde_json::json!({
            "command": "sleep 300",
            "blocking": true,
            "timeout_ms": 100
        });
        let cwd = std::env::current_dir().unwrap();

        let result = run_command(args, &cwd).await.unwrap();
        assert!(!result.success);
        assert!(result.output.contains("timed out"));
        let meta = result.metadata.unwrap();
        assert_eq!(meta["timed_out"], true);
        assert_eq!(meta["timeout_ms"], 100);
    }

    #[tokio::test]
    async fn blocking_command_uses_default_timeout_when_none_provided() {
        let args = serde_json::json!({
            "command": "echo 'fast'",
            "blocking": true
        });
        let cwd = std::env::current_dir().unwrap();

        let result = run_command(args, &cwd).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("fast"));
    }

    #[tokio::test]
    async fn nonblocking_command_metadata_includes_wall_timeout() {
        let args = serde_json::json!({
            "command": "echo 'wall'",
            "blocking": false
        });
        let cwd = std::env::current_dir().unwrap();

        let result = run_command(args, &cwd).await.unwrap();
        let meta = result.metadata.unwrap();
        assert_eq!(meta["wall_timeout_ms"], DEFAULT_NONBLOCKING_WALL_MS);
    }

    #[tokio::test]
    async fn poll_command_includes_timed_out_field() {
        let args = serde_json::json!({
            "command": "echo 'poll timeout field'",
            "blocking": false
        });
        let cwd = std::env::current_dir().unwrap();

        let result = run_command(args, &cwd).await.unwrap();
        let command_id = result.metadata.unwrap()["command_id"]
            .as_str()
            .unwrap()
            .to_string();

        tokio::time::sleep(Duration::from_millis(500)).await;

        let poll_args = serde_json::json!({"command_id": command_id});
        let poll_result = poll_command(poll_args).await.unwrap();
        let poll_meta = poll_result.metadata.unwrap();
        assert_eq!(poll_meta["timed_out"], false);
    }

    #[tokio::test]
    async fn cancel_command_marks_nonblocking_command_cancelled() {
        let args = serde_json::json!({
            "command": "sleep 30",
            "blocking": false
        });
        let cwd = std::env::current_dir().unwrap();

        let result = run_command(args, &cwd).await.unwrap();
        let command_id = result.metadata.unwrap()["command_id"]
            .as_str()
            .unwrap()
            .to_string();

        let cancel_result = cancel_command(serde_json::json!({
            "command_id": command_id
        }))
        .await
        .unwrap();
        assert!(cancel_result.success);
        assert!(cancel_result.output.contains("cancelled"));

        let poll_result = poll_command(serde_json::json!({
            "command_id": command_id
        }))
        .await
        .unwrap();
        let poll_meta = poll_result.metadata.unwrap();
        assert_eq!(poll_meta["running"], false);
        assert_eq!(poll_meta["cancelled"], true);

        tokio::time::sleep(Duration::from_millis(100)).await;
        let settled = poll_command(serde_json::json!({
            "command_id": command_id
        }))
        .await
        .unwrap();
        let settled_meta = settled.metadata.unwrap();
        assert_eq!(settled_meta["running"], false);
        assert_eq!(settled_meta["cancelled"], true);
        assert_eq!(settled_meta["exit_code"].as_i64(), Some(130));
    }

    #[tokio::test]
    async fn run_command_rejects_cwd_outside_workspace() {
        let args = serde_json::json!({
            "command": "echo 'should not run'",
            "blocking": true,
            "cwd": "/etc"
        });
        let cwd = std::env::current_dir().unwrap();
        let result = run_command(args, &cwd).await.unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("outside the workspace"),
            "expected workspace boundary error, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn run_command_rejects_cwd_traversal_attack() {
        let args = serde_json::json!({
            "command": "echo 'should not run'",
            "blocking": true,
            "cwd": "../../.."
        });
        let cwd = std::env::current_dir().unwrap();
        let result = run_command(args, &cwd).await.unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("outside the workspace"),
            "expected workspace boundary error, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn run_command_accepts_cwd_inside_workspace() {
        let args = serde_json::json!({
            "command": "echo 'inside workspace'",
            "blocking": true,
            "cwd": "src"
        });
        let cwd = std::env::current_dir().unwrap();
        let result = run_command(args, &cwd).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("inside workspace"));
    }

    #[tokio::test]
    async fn run_command_default_cwd_is_workspace_root() {
        let args = serde_json::json!({
            "command": "echo 'default cwd'",
            "blocking": true
        });
        let cwd = std::env::current_dir().unwrap();
        let result = run_command(args, &cwd).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("default cwd"));
    }

    #[tokio::test]
    async fn nonblocking_command_applies_rtk_filter() {
        let args = serde_json::json!({
            "command": "echo 'nonblocking filter'",
            "blocking": false
        });
        let cwd = std::env::current_dir().unwrap();
        let result = run_command(args, &cwd).await.unwrap();
        let meta = result.metadata.unwrap();
        assert!(meta["executed"].is_string());
        assert!(!meta["executed"].as_str().unwrap().is_empty());
    }
}
