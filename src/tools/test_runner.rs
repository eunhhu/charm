use crate::core::ToolResult;
use anyhow::Context;
use serde_json::Value;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

pub async fn run_tests(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let test_framework = args
        .get("framework")
        .and_then(|v| v.as_str())
        .unwrap_or("auto"); // auto, cargo, jest, pytest

    let test_pattern = args.get("pattern").and_then(|v| v.as_str());

    // Handle auto-detection
    if test_framework == "auto" || test_framework.is_empty() {
        if cwd.join("Cargo.toml").exists() {
            return Box::pin(run_tests(
                serde_json::json!({ "framework": "cargo", "pattern": test_pattern }),
                cwd,
            ))
            .await;
        } else if cwd.join("package.json").exists() {
            return Box::pin(run_tests(
                serde_json::json!({ "framework": "jest", "pattern": test_pattern }),
                cwd,
            ))
            .await;
        } else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Could not auto-detect test framework. Please specify 'framework' parameter (cargo, pytest, or jest)".to_string()),
                metadata: None,
            });
        }
    }

    let mut cmd_args: Vec<String> = Vec::new();

    match test_framework {
        "cargo" => {
            cmd_args.push("test".to_string());
            if let Some(pattern) = test_pattern {
                cmd_args.push(pattern.to_string());
            }
            cmd_args.push("--".to_string());
            cmd_args.push("--nocapture".to_string());
        }
        "pytest" => {
            cmd_args.push("-m".to_string());
            cmd_args.push("pytest".to_string());
            if let Some(pattern) = test_pattern {
                cmd_args.push("-k".to_string());
                cmd_args.push(pattern.to_string());
            }
            cmd_args.push("-v".to_string());
        }
        "jest" => {
            cmd_args.push("npx".to_string());
            cmd_args.push("jest".to_string());
            if let Some(pattern) = test_pattern {
                cmd_args.push(pattern.to_string());
            }
            cmd_args.push("--verbose".to_string());
        }
        _ => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown test framework: {}", test_framework)),
                metadata: None,
            });
        }
    }

    let (program, args) = if test_framework == "jest" {
        ("npx", cmd_args[1..].to_vec())
    } else if test_framework == "pytest" {
        ("python", cmd_args)
    } else {
        ("cargo", cmd_args)
    };

    let mut child = tokio::process::Command::new(program)
        .args(&args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context(format!("Failed to spawn {} process", program))?;

    let stdout = child.stdout.take().context("Failed to capture stdout")?;
    let stderr = child.stderr.take().context("Failed to capture stderr")?;

    let mut stdout_reader = BufReader::new(stdout).lines();
    let mut stderr_reader = BufReader::new(stderr).lines();

    let mut output = String::new();
    let mut error_output = String::new();

    // Read stdout
    while let Some(line) = stdout_reader.next_line().await? {
        output.push_str(&line);
        output.push('\n');
    }

    // Read stderr
    while let Some(line) = stderr_reader.next_line().await? {
        error_output.push_str(&line);
        error_output.push('\n');
    }

    let status = child.wait().await?;
    let success = status.success();

    // Combine outputs
    let full_output = if error_output.is_empty() {
        output.clone()
    } else {
        format!("{output}\n{error_output}")
    };

    Ok(ToolResult {
        success,
        output: full_output,
        error: if success {
            None
        } else {
            Some(format!("Tests failed with exit code: {:?}", status.code()))
        },
        metadata: Some(serde_json::json!({
            "framework": test_framework,
            "exit_code": status.code()
        })),
    })
}

pub async fn analyze_test_results(args: Value, _cwd: &Path) -> anyhow::Result<ToolResult> {
    let test_output = args
        .get("output")
        .and_then(|v| v.as_str())
        .context("Missing 'output' parameter")?;

    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;

    // Simple parsing for common test output patterns
    for line in test_output.lines() {
        let line_lower = line.to_lowercase();
        if (line_lower.contains("test result: ok") || line_lower.contains("passed"))
            && let Some(n) = extract_number_before(&line_lower, "passed")
        {
            passed += n;
        }
        if (line_lower.contains("test result: failed") || line_lower.contains("failed"))
            && let Some(n) = extract_number_before(&line_lower, "failed")
        {
            failed += n;
        }
        if line_lower.contains("skipped")
            && let Some(n) = extract_number_before(&line_lower, "skipped")
        {
            skipped += n;
        }
    }

    let summary = format!(
        "Test Analysis:\n- Passed: {}\n- Failed: {}\n- Skipped: {}\n\nSuccess Rate: {:.1}%",
        passed,
        failed,
        skipped,
        if passed + failed > 0 {
            (passed as f64 / (passed + failed) as f64) * 100.0
        } else {
            0.0
        }
    );

    Ok(ToolResult {
        success: failed == 0,
        output: summary,
        error: if failed > 0 {
            Some(format!("{} tests failed", failed))
        } else {
            None
        },
        metadata: Some(serde_json::json!({
            "passed": passed,
            "failed": failed,
            "skipped": skipped
        })),
    })
}

fn extract_number_before(text: &str, keyword: &str) -> Option<i32> {
    if let Some(pos) = text.find(keyword) {
        let before = &text[..pos];
        // Find the last number before the keyword
        let words: Vec<&str> = before.split_whitespace().rev().collect();
        for word in words {
            if let Ok(n) = word.parse::<i32>() {
                return Some(n);
            }
        }
    }
    None
}
