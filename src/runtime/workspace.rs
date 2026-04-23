use crate::indexer::store::IndexStore;
use crate::runtime::types::{
    DiagnosticSummary, LspServerSnapshot, LspSnapshot, SymbolJump, WorkspacePreflight,
};
use anyhow::Context;
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub fn build_preflight(
    workspace_root: &Path,
    branch: String,
    dirty_files: Vec<String>,
    recent_summary: Option<String>,
) -> WorkspacePreflight {
    let mut suggested_actions = Vec::new();
    if dirty_files.is_empty() {
        suggested_actions.push("Inspect workspace and gather context".to_string());
    } else {
        suggested_actions.push("Review dirty files before editing".to_string());
    }
    if workspace_root.join("Cargo.toml").exists() {
        suggested_actions.push("Run focused cargo test before broad changes".to_string());
    }

    WorkspacePreflight {
        branch,
        dirty_files,
        recent_summary,
        suggested_actions,
    }
}

pub fn collect_lsp_snapshot(workspace_root: &Path) -> LspSnapshot {
    collect_lsp_snapshot_with_path(workspace_root, None)
}

pub async fn refresh_lsp_snapshot(workspace_root: &Path) -> anyhow::Result<LspSnapshot> {
    refresh_lsp_snapshot_with_path(workspace_root, None).await
}

fn collect_lsp_snapshot_with_path(
    workspace_root: &Path,
    path_override: Option<&str>,
) -> LspSnapshot {
    let mut active_roots = Vec::new();
    let mut servers = Vec::new();

    if workspace_root.join("Cargo.toml").exists() {
        active_roots.push("rust".to_string());
        servers.push(resolve_server("rust", &["rust-analyzer"], path_override));
    }
    if workspace_root.join("package.json").exists() || workspace_root.join("tsconfig.json").exists()
    {
        active_roots.push("typescript".to_string());
        servers.push(resolve_server(
            "typescript",
            &["typescript-language-server", "vtsls"],
            path_override,
        ));
    }
    if workspace_root.join("pyproject.toml").exists()
        || workspace_root.join("requirements.txt").exists()
        || workspace_root.join("setup.py").exists()
    {
        active_roots.push("python".to_string());
        servers.push(resolve_server(
            "python",
            &["pyright-langserver", "pylsp", "jedi-language-server"],
            path_override,
        ));
    }

    let diagnostics = load_cached_diagnostics(workspace_root);
    let symbol_jumps = load_symbol_jumps(workspace_root, 12);
    let has_symbol_index = IndexStore::new(workspace_root).exists();

    LspSnapshot {
        ready: servers.iter().any(|server| server.ready),
        active_roots,
        diagnostics,
        symbol_provider: if has_symbol_index {
            "semantic-index".to_string()
        } else {
            "none".to_string()
        },
        servers,
        symbol_jumps,
    }
}

async fn refresh_lsp_snapshot_with_path(
    workspace_root: &Path,
    path_override: Option<&str>,
) -> anyhow::Result<LspSnapshot> {
    let diagnostics = collect_live_diagnostics(workspace_root, path_override).await?;
    persist_diagnostics(workspace_root, &diagnostics)?;
    Ok(collect_lsp_snapshot_with_path(
        workspace_root,
        path_override,
    ))
}

fn resolve_server(
    language: &str,
    candidates: &[&str],
    path_override: Option<&str>,
) -> LspServerSnapshot {
    let command = candidates
        .iter()
        .find(|candidate| command_available(candidate, path_override))
        .copied()
        .unwrap_or_else(|| candidates.first().copied().unwrap_or("unknown"));

    LspServerSnapshot {
        language: language.to_string(),
        command: command.to_string(),
        ready: command_available(command, path_override),
    }
}

fn load_cached_diagnostics(workspace_root: &Path) -> Vec<DiagnosticSummary> {
    let path = workspace_root.join(".charm").join("diagnostics.json");
    if !path.exists() {
        return Vec::new();
    }

    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<DiagnosticSummary>>(&raw).ok())
        .unwrap_or_default()
}

fn persist_diagnostics(
    workspace_root: &Path,
    diagnostics: &[DiagnosticSummary],
) -> anyhow::Result<()> {
    let dir = workspace_root.join(".charm");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("diagnostics.json"),
        serde_json::to_string_pretty(diagnostics)?,
    )?;
    Ok(())
}

fn load_symbol_jumps(workspace_root: &Path, limit: usize) -> Vec<SymbolJump> {
    let store = IndexStore::new(workspace_root);
    let Ok(index) = store.load() else {
        return Vec::new();
    };

    let mut symbols = index.symbols;
    symbols.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then(left.line.cmp(&right.line))
            .then(left.name.cmp(&right.name))
    });

    symbols
        .into_iter()
        .take(limit)
        .map(|symbol| SymbolJump {
            name: symbol.name,
            file_path: symbol.file_path,
            line: symbol.line,
        })
        .collect()
}

async fn collect_live_diagnostics(
    workspace_root: &Path,
    path_override: Option<&str>,
) -> anyhow::Result<Vec<DiagnosticSummary>> {
    let mut diagnostics = Vec::new();

    if workspace_root.join("Cargo.toml").exists() && command_available("cargo", path_override) {
        diagnostics.extend(collect_rust_diagnostics(workspace_root, path_override).await?);
    }

    if (workspace_root.join("package.json").exists()
        || workspace_root.join("tsconfig.json").exists())
        && command_available("tsc", path_override)
    {
        diagnostics.extend(collect_typescript_diagnostics(workspace_root, path_override).await?);
    }

    if (workspace_root.join("pyproject.toml").exists()
        || workspace_root.join("requirements.txt").exists()
        || workspace_root.join("setup.py").exists())
        && command_available("pyright", path_override)
    {
        diagnostics.extend(collect_python_diagnostics(workspace_root, path_override).await?);
    }

    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.message.cmp(&right.message))
    });
    diagnostics.dedup_by(|left, right| left.path == right.path && left.message == right.message);
    Ok(diagnostics)
}

async fn collect_rust_diagnostics(
    workspace_root: &Path,
    path_override: Option<&str>,
) -> anyhow::Result<Vec<DiagnosticSummary>> {
    let output = run_command_capture(
        workspace_root,
        "cargo",
        &["check", "--message-format=json", "--quiet"],
        path_override,
    )
    .await?;

    let mut diagnostics = Vec::new();
    for line in output.stdout.lines() {
        let Ok(payload) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if payload.get("reason").and_then(Value::as_str) != Some("compiler-message") {
            continue;
        }

        let message = payload
            .get("message")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let text = message
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("compiler diagnostic");
        let level = message
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("info");
        let file_name = message
            .get("spans")
            .and_then(Value::as_array)
            .and_then(|spans| {
                spans
                    .iter()
                    .find_map(|span| span.get("file_name").and_then(Value::as_str))
            })
            .unwrap_or("workspace");

        diagnostics.push(DiagnosticSummary {
            path: normalize_path(workspace_root, file_name),
            message: format!("{level}: {text}"),
        });
    }

    Ok(diagnostics)
}

async fn collect_typescript_diagnostics(
    workspace_root: &Path,
    path_override: Option<&str>,
) -> anyhow::Result<Vec<DiagnosticSummary>> {
    let output = run_command_capture(
        workspace_root,
        "tsc",
        &["--noEmit", "--pretty", "false"],
        path_override,
    )
    .await?;

    Ok(output
        .stdout
        .lines()
        .chain(output.stderr.lines())
        .filter_map(|line| parse_typescript_diagnostic(line, workspace_root))
        .collect())
}

async fn collect_python_diagnostics(
    workspace_root: &Path,
    path_override: Option<&str>,
) -> anyhow::Result<Vec<DiagnosticSummary>> {
    let output =
        run_command_capture(workspace_root, "pyright", &["--outputjson"], path_override).await?;
    let payload: Value = serde_json::from_str(&output.stdout)
        .or_else(|_| serde_json::from_str(&output.stderr))
        .context("failed to parse pyright diagnostic output")?;

    Ok(payload
        .get("generalDiagnostics")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|diagnostic| {
            let path = diagnostic.get("file").and_then(Value::as_str)?;
            let message = diagnostic.get("message").and_then(Value::as_str)?;
            Some(DiagnosticSummary {
                path: normalize_path(workspace_root, path),
                message: message.to_string(),
            })
        })
        .collect())
}

fn parse_typescript_diagnostic(line: &str, workspace_root: &Path) -> Option<DiagnosticSummary> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some((prefix, message)) = trimmed.split_once(" - error TS") {
        let path = prefix.split(':').next()?;
        return Some(DiagnosticSummary {
            path: normalize_path(workspace_root, path),
            message: format!("error TS{}", message),
        });
    }

    if let Some((prefix, message)) = trimmed.split_once("): error TS") {
        let path = prefix.split('(').next()?;
        return Some(DiagnosticSummary {
            path: normalize_path(workspace_root, path),
            message: format!("error TS{}", message),
        });
    }

    None
}

struct CommandCapture {
    stdout: String,
    stderr: String,
}

async fn run_command_capture(
    workspace_root: &Path,
    command: &str,
    args: &[&str],
    path_override: Option<&str>,
) -> anyhow::Result<CommandCapture> {
    let mut child = Command::new(command);
    child.args(args).current_dir(workspace_root);
    if let Some(path) = path_override {
        child.env("PATH", path);
    }

    let output = child
        .output()
        .await
        .with_context(|| format!("failed to run {command}"))?;

    Ok(CommandCapture {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn normalize_path(workspace_root: &Path, raw_path: &str) -> String {
    let path = Path::new(raw_path);
    let relative = if path.is_absolute() {
        path.strip_prefix(workspace_root).unwrap_or(path)
    } else {
        path
    };

    relative.to_string_lossy().replace('\\', "/")
}

fn command_available(command: &str, path_override: Option<&str>) -> bool {
    if command.contains(std::path::MAIN_SEPARATOR) {
        return PathBuf::from(command).is_file();
    }

    let path_var = path_override
        .map(OsString::from)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();

    std::env::split_paths(&path_var).any(|dir| executable_exists(&dir, command))
}

fn executable_exists(dir: &Path, command: &str) -> bool {
    let candidate = dir.join(command);
    if candidate.is_file() {
        return true;
    }

    #[cfg(windows)]
    {
        let exe = dir.join(format!("{command}.exe"));
        if exe.is_file() {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::store::IndexStore;
    use crate::indexer::types::{Index, Symbol};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn cargo_workspace_gets_rust_preflight_hint() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='demo'\nversion='0.1.0'\n",
        )
        .unwrap();
        let preflight = build_preflight(dir.path(), "main".to_string(), vec![], None);
        assert!(
            preflight
                .suggested_actions
                .iter()
                .any(|item| item.contains("cargo test"))
        );
        let lsp = collect_lsp_snapshot(dir.path());
        assert_eq!(lsp.active_roots, vec!["rust".to_string()]);
    }

    #[test]
    fn lsp_snapshot_uses_real_server_and_cached_workspace_state() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='demo'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join(".charm")).unwrap();
        fs::write(
            dir.path().join(".charm").join("diagnostics.json"),
            serde_json::to_string(&vec![DiagnosticSummary {
                path: "src/main.rs".to_string(),
                message: "unused variable".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();

        let mut index = Index::default();
        index.add_symbol(Symbol {
            name: "run_session".to_string(),
            kind: "function".to_string(),
            file_path: "src/main.rs".to_string(),
            line: 42,
            col: 1,
            signature: "fn run_session()".to_string(),
            docstring: None,
            body_start: 42,
            body_end: 60,
        });
        IndexStore::new(dir.path()).save(&index).unwrap();

        let bin_dir = tempdir().unwrap();
        let rust_analyzer = bin_dir.path().join("rust-analyzer");
        fs::write(&rust_analyzer, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = fs::metadata(&rust_analyzer).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&rust_analyzer, permissions).unwrap();

        let lsp = collect_lsp_snapshot_with_path(
            dir.path(),
            Some(bin_dir.path().to_string_lossy().as_ref()),
        );

        assert!(lsp.ready);
        assert_eq!(lsp.diagnostics.len(), 1);
        assert_eq!(lsp.symbol_jumps.len(), 1);
        assert!(
            lsp.servers
                .iter()
                .any(|server| server.language == "rust" && server.ready)
        );
    }

    #[tokio::test]
    async fn refresh_lsp_snapshot_collects_rust_check_diagnostics() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='demo'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src").join("main.rs"), "fn main() {}\n").unwrap();

        let bin_dir = tempdir().unwrap();
        let cargo = bin_dir.path().join("cargo");
        fs::write(
            &cargo,
            "#!/bin/sh\nprintf '%s\\n' '{\"reason\":\"compiler-message\",\"message\":{\"level\":\"warning\",\"message\":\"unused variable\",\"spans\":[{\"file_name\":\"src/main.rs\"}]}}'\nexit 1\n",
        )
        .unwrap();
        let rust_analyzer = bin_dir.path().join("rust-analyzer");
        fs::write(&rust_analyzer, "#!/bin/sh\nexit 0\n").unwrap();

        for path in [&cargo, &rust_analyzer] {
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).unwrap();
        }

        let snapshot = refresh_lsp_snapshot_with_path(
            dir.path(),
            Some(bin_dir.path().to_string_lossy().as_ref()),
        )
        .await
        .expect("refresh");

        assert!(snapshot.ready);
        assert_eq!(snapshot.diagnostics.len(), 1);
        assert_eq!(snapshot.diagnostics[0].path, "src/main.rs");
        assert!(snapshot.diagnostics[0].message.contains("unused variable"));
    }
}
