use crate::core::ToolResult;
use std::collections::HashMap;
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref RTK_SAVINGS: Mutex<HashMap<String, (usize, usize)>> = Mutex::new(HashMap::new());
    static ref RTK_AVAILABLE: Mutex<Option<bool>> = Mutex::new(None);
}

pub fn is_rtk_available() -> bool {
    let mut cache = RTK_AVAILABLE.lock().unwrap();
    if let Some(v) = *cache {
        return v;
    }
    let available = std::process::Command::new("rtk")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    *cache = Some(available);
    available
}

pub fn rewrite_with_rtk(command: &str) -> Option<String> {
    if !is_rtk_available() {
        return None;
    }

    let trimmed = command.trim();
    let first_word = trimmed.split_whitespace().next()?;

    let rtk_commands = [
        "git",
        "cargo",
        "npm",
        "pnpm",
        "yarn",
        "docker",
        "kubectl",
        "pytest",
        "python",
        "node",
        "go",
        "ruff",
        "tsc",
        "next",
        "jest",
        "vitest",
        "playwright",
        "rspec",
        "rake",
        "bundle",
        "pip",
        "gh",
        "aws",
        "curl",
        "wget",
        "ls",
        "cat",
        "find",
        "grep",
        "diff",
        "tree",
        "env",
    ];

    if rtk_commands.contains(&first_word) {
        return Some(format!("rtk {}", trimmed));
    }

    if trimmed.starts_with("cargo test")
        || trimmed.starts_with("cargo build")
        || trimmed.starts_with("cargo clippy")
    {
        return Some(format!("rtk {}", trimmed));
    }

    None
}

pub async fn filter_with_rtk(output: &str, command_hint: &str) -> String {
    if !is_rtk_available() || output.len() < 500 {
        return fallback_compress(output, command_hint);
    }

    let mut child = match tokio::process::Command::new("rtk")
        .arg("summary")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return fallback_compress(output, command_hint),
    };

    if let Some(mut stdin) = child.stdin.take() {
        let output = output.to_string();
        tokio::task::spawn(async move {
            let _ = tokio::io::AsyncWriteExt::write_all(&mut stdin, output.as_bytes()).await;
        });
    }

    match tokio::time::timeout(
        tokio::time::Duration::from_secs(2),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(result)) if result.status.success() => {
            let filtered = String::from_utf8_lossy(&result.stdout).to_string();
            track_savings(command_hint, output.len(), filtered.len());
            if filtered.len() < output.len() / 2 {
                filtered
            } else {
                fallback_compress(output, command_hint)
            }
        }
        _ => fallback_compress(output, command_hint),
    }
}

pub fn fallback_compress(output: &str, command_hint: &str) -> String {
    if output.len() < 1000 {
        return output.to_string();
    }

    let lines: Vec<&str> = output.lines().collect();

    if command_hint.contains("test")
        || command_hint.contains("cargo test")
        || command_hint.contains("pytest")
    {
        return compress_test_output(output, &lines);
    }

    if command_hint.contains("git status") || command_hint.contains("git diff") {
        return compress_git_output(output, &lines);
    }

    if command_hint.contains("docker") || command_hint.contains("kubectl") {
        return compress_table_output(output, &lines);
    }

    if lines.len() > 200 {
        let head = lines
            .iter()
            .take(50)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let tail = lines
            .iter()
            .skip(lines.len() - 50)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let compressed = format!(
            "{}\n... ({} lines omitted) ...\n{}",
            head,
            lines.len() - 100,
            tail
        );
        track_savings(command_hint, output.len(), compressed.len());
        compressed
    } else {
        output.to_string()
    }
}

fn compress_test_output(output: &str, lines: &[&str]) -> String {
    let mut failures = Vec::new();
    let mut summary = Vec::new();
    let mut passed = 0usize;
    let mut failed = 0usize;

    for line in lines {
        let l = line.trim();
        if l.contains("FAILED")
            || l.contains("failure")
            || l.contains("panic")
            || l.contains("assertion failed")
        {
            failures.push(*line);
        } else if l.contains("test result:")
            || l.contains("running ")
            || l.contains("test ") && l.contains("... ok")
        {
            summary.push(*line);
        } else if l.contains("... ok") {
            passed += 1;
        } else if l.contains("... FAILED") || l.contains("... failed") {
            failed += 1;
        }
    }

    let mut result = Vec::new();
    if !summary.is_empty() {
        result.push("=== Summary ===".to_string());
        result.extend(summary.iter().map(|s| s.to_string()));
    }
    result.push(format!("passed: {}, failed: {}", passed, failed));

    if !failures.is_empty() {
        result.push("\n=== Failures ===".to_string());
        result.extend(failures.iter().map(|s| s.to_string()).take(30));
        if failures.len() > 30 {
            result.push(format!("... and {} more failures", failures.len() - 30));
        }
    }

    let compressed = result.join("\n");
    track_savings("test", output.len(), compressed.len());
    compressed
}

fn compress_git_output(output: &str, lines: &[&str]) -> String {
    let meaningful: Vec<String> = lines
        .iter()
        .filter(|l| {
            let s = l.trim();
            !s.is_empty()
                && !s.starts_with("index ")
                && !s.starts_with("diff --git")
                && !s.starts_with("--- ")
                && !s.starts_with("+++ ")
                && !s.starts_with("@@")
        })
        .map(|s| s.to_string())
        .collect();

    let compressed = meaningful.join("\n");
    track_savings("git", output.len(), compressed.len());
    compressed
}

fn compress_table_output(output: &str, lines: &[&str]) -> String {
    if lines.len() <= 50 {
        return output.to_string();
    }
    let head = lines
        .iter()
        .take(20)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let tail = lines
        .iter()
        .skip(lines.len() - 10)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let compressed = format!("{}\n... ({} rows) ...\n{}", head, lines.len() - 30, tail);
    track_savings("table", output.len(), compressed.len());
    compressed
}

fn track_savings(command_hint: &str, original: usize, filtered: usize) {
    if filtered >= original {
        return;
    }
    let mut store = RTK_SAVINGS.lock().unwrap();
    let entry = store.entry(command_hint.to_string()).or_insert((0, 0));
    entry.0 += original;
    entry.1 += filtered;
}

pub fn get_savings_report() -> String {
    let store = RTK_SAVINGS.lock().unwrap();
    if store.is_empty() {
        return "No RTK savings recorded yet.".to_string();
    }

    let mut total_original = 0usize;
    let mut total_filtered = 0usize;
    let mut lines = vec!["RTK Token Savings:".to_string()];

    for (cmd, (orig, filt)) in store.iter() {
        total_original += orig;
        total_filtered += filt;
        let pct = if *orig > 0 {
            ((orig - filt) * 100 / orig) as u32
        } else {
            0
        };
        lines.push(format!("  {}: {} -> {} ({}% saved)", cmd, orig, filt, pct));
    }

    let total_pct = if total_original > 0 {
        ((total_original - total_filtered) * 100 / total_original) as u32
    } else {
        0
    };
    lines.push(format!(
        "Total: {} -> {} ({}% saved)",
        total_original, total_filtered, total_pct
    ));

    lines.join("\n")
}

pub fn apply_to_tool_result(result: &mut ToolResult, command_hint: &str) {
    if result.output.len() < 500 {
        return;
    }
    let filtered = tokio::runtime::Handle::try_current()
        .map(|rt| rt.block_on(filter_with_rtk(&result.output, command_hint)))
        .unwrap_or_else(|_| fallback_compress(&result.output, command_hint));
    result.output = filtered;
}
