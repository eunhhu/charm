use crate::runtime::types::{DiagnosticSummary, LspSnapshot, WorkspacePreflight};
use std::path::Path;

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
    let mut active_roots = Vec::new();
    if workspace_root.join("Cargo.toml").exists() {
        active_roots.push("rust".to_string());
    }
    if workspace_root.join("package.json").exists() {
        active_roots.push("typescript".to_string());
    }

    LspSnapshot {
        ready: true,
        active_roots,
        diagnostics: Vec::<DiagnosticSummary>::new(),
        symbol_provider: "semantic-index".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
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
}
