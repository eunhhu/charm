use crate::core::ToolResult;
use serde_json::Value;
use std::path::Path;

mod browser;
mod cache;
mod command;
pub(crate) mod fast_executor;
mod file;
mod github_cli;
mod memory_tools;
mod notebook;
mod patch;
mod retrieval;
pub mod rtk_filter;
pub mod search;
mod semantic;
mod test_runner;
mod todo;
mod trajectory;
pub(crate) mod url_guard;
mod web;
mod web_search;

use cache::ToolCache;
pub(crate) use fast_executor::FastExecutor;

pub struct ToolRegistry {
    cwd: std::path::PathBuf,
    cache: ToolCache,
    file_cache: fast_executor::FileCache,
}

impl ToolRegistry {
    pub fn new(cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            cache: ToolCache::new(50),
            file_cache: fast_executor::FileCache::with_persistence(
                10,
                cwd.join(".charm").join("cache").join("file-cache.json"),
            ),
        }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn list_tools(&self) -> Vec<&'static str> {
        vec![
            "read_range",
            "write_file",
            "grep_search",
            "glob_search",
            "list_dir",
            "edit_patch",
            "semantic_search",
            "parallel_search",
            "run_command",
            "poll_command",
            "cancel_command",
            "fetch_url",
            "http_request",
            "browser_navigate",
            "browser_screenshot",
            "browser_click",
            "browser_type",
            "browser_evaluate",
            "browser_snapshot",
            "browser_close",
            "todo_add",
            "todo_list",
            "todo_update",
            "todo_delete",
            "todo_clear",
            "read_notebook",
            "read_notebook_cell",
            "edit_notebook_cell",
            "insert_notebook_cell",
            "trajectory_search",
            "trajectory_list",
            "trajectory_get_context",
            "trajectory_clear",
            "search_web",
            "fetch_search_result",
            "github_pr_list",
            "github_pr_view",
            "github_issue_list",
            "github_issue_view",
            "github_repo_view",
            "run_tests",
            "analyze_test_results",
            "checkpoint_create",
            "checkpoint_restore",
            "plan_update",
            "memory_stage",
            "memory_commit",
        ]
    }

    pub async fn execute(&mut self, tool: &str, args: Value) -> anyhow::Result<ToolResult> {
        if ToolCache::is_cachable(tool) {
            if let Some(cached) = self.cache.get(tool, &args) {
                return Ok(cached);
            }
            let result = self.execute_uncached(tool, args.clone()).await?;
            self.cache.put(tool, &args, result.clone());
            return Ok(result);
        }
        self.execute_uncached(tool, args).await
    }

    async fn execute_uncached(&mut self, tool: &str, args: Value) -> anyhow::Result<ToolResult> {
        let hint = args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or(tool)
            .to_string();
        let mut result = match tool {
            "read_range" => {
                file::read_range_with_cache(args, &self.cwd, &mut self.file_cache).await
            }
            "write_file" => {
                let result = file::write_file(args, &self.cwd).await?;
                if result.success {
                    if let Some(path) = result
                        .metadata
                        .as_ref()
                        .and_then(|meta| meta.get("resolved_path"))
                        .and_then(|path| path.as_str())
                    {
                        self.file_cache.invalidate(Path::new(path));
                    }
                }
                Ok(result)
            }
            "grep_search" => search::grep_search(args, &self.cwd).await,
            "glob_search" => search::glob_search(args, &self.cwd).await,
            "list_dir" => search::list_dir(args, &self.cwd).await,
            "edit_patch" => patch::edit_patch(args, &self.cwd).await,
            "semantic_search" => semantic::semantic_search(args, &self.cwd).await,
            "parallel_search" => retrieval::parallel_search(args, &self.cwd).await,
            "run_command" => command::run_command(args, &self.cwd).await,
            "poll_command" => command::poll_command(args).await,
            "cancel_command" => command::cancel_command(args).await,
            "fetch_url" => web::fetch_url(args).await,
            "http_request" => web::http_request(args).await,
            "browser_navigate" => browser::browser_navigate(args).await,
            "browser_screenshot" => browser::browser_screenshot(args).await,
            "browser_click" => browser::browser_click(args).await,
            "browser_type" => browser::browser_type(args).await,
            "browser_evaluate" => browser::browser_evaluate(args).await,
            "browser_snapshot" => browser::browser_snapshot(args).await,
            "browser_close" => browser::browser_close(args).await,
            "todo_add" => todo::todo_add(args, &self.cwd).await,
            "todo_list" => todo::todo_list_items(args, &self.cwd).await,
            "todo_update" => todo::todo_update(args, &self.cwd).await,
            "todo_delete" => todo::todo_delete(args, &self.cwd).await,
            "todo_clear" => todo::todo_clear(args, &self.cwd).await,
            "read_notebook" => notebook::read_notebook(args, &self.cwd).await,
            "read_notebook_cell" => notebook::read_notebook_cell(args, &self.cwd).await,
            "edit_notebook_cell" => notebook::edit_notebook_cell(args, &self.cwd).await,
            "insert_notebook_cell" => notebook::insert_notebook_cell(args, &self.cwd).await,
            "trajectory_search" => trajectory::trajectory_search(args).await,
            "trajectory_list" => trajectory::trajectory_list(args).await,
            "trajectory_get_context" => trajectory::trajectory_get_context(args).await,
            "trajectory_clear" => trajectory::trajectory_clear(args).await,
            "search_web" => web_search::search_web(args).await,
            "fetch_search_result" => web_search::fetch_search_result(args).await,
            "github_pr_list" => github_cli::github_pr_list(args, &self.cwd).await,
            "github_pr_view" => github_cli::github_pr_view(args, &self.cwd).await,
            "github_issue_list" => github_cli::github_issue_list(args, &self.cwd).await,
            "github_issue_view" => github_cli::github_issue_view(args, &self.cwd).await,
            "github_repo_view" => github_cli::github_repo_view(args, &self.cwd).await,
            "run_tests" => test_runner::run_tests(args, &self.cwd).await,
            "analyze_test_results" => test_runner::analyze_test_results(args, &self.cwd).await,
            "checkpoint_create" => {
                let mut cm = crate::harness::CheckpointManager::new(&self.cwd)?;
                cm.create(args)
            }
            "checkpoint_restore" => {
                let mut cm = crate::harness::CheckpointManager::new(&self.cwd)?;
                cm.restore(args)
            }
            "plan_update" => {
                let pm = crate::harness::PlanManager::new(&self.cwd);
                pm.update(args)
            }
            "memory_stage" => memory_tools::memory_stage(args, &self.cwd).await,
            "memory_commit" => memory_tools::memory_commit(args, &self.cwd).await,
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown tool: {}", tool)),
                metadata: None,
            }),
        }?;

        match tool {
            "run_command" | "poll_command" | "cancel_command" => {
                let filtered = rtk_filter::filter_with_rtk(&result.output, &hint).await;
                if filtered.len() < result.output.len() {
                    result.output = filtered;
                    mark_output_truncated(&mut result);
                }
            }
            "grep_search" | "glob_search" | "semantic_search" | "parallel_search" => {
                if result.output.len() > 2000 {
                    let filtered = rtk_filter::fallback_compress(&result.output, tool);
                    result.output = filtered;
                }
            }
            _ => {}
        }

        Ok(result)
    }
}

fn mark_output_truncated(result: &mut ToolResult) {
    let metadata = result.metadata.get_or_insert_with(|| serde_json::json!({}));
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("output_truncated".to_string(), serde_json::json!(true));
        obj.insert(
            "rendered_output_bytes".to_string(),
            serde_json::json!(result.output.len()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All tool names exposed in default_tool_schemas must be parsable by ToolParser
    #[test]
    fn test_schema_tools_are_parsable() {
        let schema_tools = crate::providers::types::default_tool_schemas();
        let registry = ToolRegistry::new(Path::new("."));
        let registry_tools = registry.list_tools();

        for schema in &schema_tools {
            let tool_name = &schema.function.name;
            assert!(
                registry_tools.contains(&tool_name.as_str()),
                "Schema tool '{}' not found in registry list_tools",
                tool_name
            );
        }
    }

    /// Core tools that must have schema + parser + enum + registry consistency
    const CORE_TOOLS: &[&str] = &[
        "read_range",
        "grep_search",
        "glob_search",
        "semantic_search",
        "parallel_search",
        "list_dir",
        "edit_patch",
        "write_file",
        "run_command",
        "poll_command",
        "cancel_command",
        "checkpoint_create",
        "checkpoint_restore",
        "plan_update",
        "memory_stage",
        "memory_commit",
    ];

    #[test]
    fn test_core_tools_in_schema() {
        let schema_tools = crate::providers::types::default_tool_schemas();
        let schema_names: std::collections::HashSet<_> = schema_tools
            .iter()
            .map(|s| s.function.name.as_str())
            .collect();

        for tool in CORE_TOOLS {
            assert!(
                schema_names.contains(tool),
                "Core tool '{}' missing from default_tool_schemas",
                tool
            );
        }
    }

    #[test]
    fn test_core_tools_in_registry() {
        let registry = ToolRegistry::new(Path::new("."));
        let registry_tools = registry.list_tools();
        let registry_set: std::collections::HashSet<_> = registry_tools.into_iter().collect();

        for tool in CORE_TOOLS {
            assert!(
                registry_set.contains(tool),
                "Core tool '{}' missing from registry list_tools",
                tool
            );
        }
    }

    /// Test that registry returns unknown tool error for non-existent tools
    #[tokio::test]
    async fn test_unknown_tool_returns_error() {
        let mut registry = ToolRegistry::new(Path::new("."));
        let result = registry
            .execute("nonexistent_tool", serde_json::json!({}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("Unknown tool"));
    }

    #[tokio::test]
    async fn read_range_uses_registry_file_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        tokio::fs::write(&path, "one\ntwo\nthree\n").await.unwrap();

        let mut registry = ToolRegistry::new(dir.path());
        let args = serde_json::json!({
            "file_path": "sample.txt",
            "offset": 1,
            "limit": 2
        });

        let first = registry.execute("read_range", args.clone()).await.unwrap();
        let second = registry.execute("read_range", args).await.unwrap();

        assert_eq!(first.metadata.unwrap()["cache_hit"], false);
        assert_eq!(second.metadata.unwrap()["cache_hit"], true);
        assert!(second.output.contains("1: one"));
    }

    #[tokio::test]
    async fn read_range_file_cache_persists_between_registries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        tokio::fs::write(&path, "one\ntwo\nthree\n").await.unwrap();
        let args = serde_json::json!({
            "file_path": "sample.txt",
            "offset": 1,
            "limit": 2
        });

        let mut first_registry = ToolRegistry::new(dir.path());
        let first = first_registry
            .execute("read_range", args.clone())
            .await
            .unwrap();
        assert_eq!(first.metadata.unwrap()["cache_hit"], false);

        let mut second_registry = ToolRegistry::new(dir.path());
        let second = second_registry.execute("read_range", args).await.unwrap();
        assert_eq!(second.metadata.unwrap()["cache_hit"], true);
        assert!(dir.path().join(".charm/cache/file-cache.json").exists());
    }

    #[tokio::test]
    async fn command_filter_marks_output_truncated_but_preserves_full_log_ref() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ToolRegistry::new(dir.path());

        let result = registry
            .execute(
                "run_command",
                serde_json::json!({
                    "command": "for i in $(seq 1 300); do echo line-$i; done",
                    "blocking": true
                }),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.len() < 3000, "output should be filtered");
        let meta = result.metadata.unwrap();
        assert_eq!(meta["output_truncated"], true);
        let log_ref = meta["log_ref"].as_str().expect("log_ref");
        let raw_log = std::fs::read_to_string(dir.path().join(log_ref)).unwrap();
        assert!(raw_log.contains("line-1"));
        assert!(raw_log.contains("line-300"));
        assert!(raw_log.len() > result.output.len());
    }

    /// Test that poll_command schema has correct parameters
    #[test]
    fn test_poll_command_schema_params() {
        let schema_tools = crate::providers::types::default_tool_schemas();
        let poll_cmd = schema_tools
            .iter()
            .find(|s| s.function.name == "poll_command")
            .expect("poll_command should be in schema");

        let params = &poll_cmd.function.parameters;
        assert!(params.get("properties").is_some());
        assert!(params.get("required").is_some());

        let props = params["properties"].as_object().unwrap();
        assert!(props.contains_key("command_id"));
        assert!(props.contains_key("output_priority"));
        assert!(props.contains_key("max_lines"));
    }

    /// Test risk_class enum values match between schema and parser
    #[test]
    fn test_risk_class_enum_consistency() {
        use crate::core::RiskClass;

        // Verify all RiskClass variants can be parsed from their kebab-case strings
        let cases = [
            ("safe-read", RiskClass::SafeRead),
            ("safe-exec", RiskClass::SafeExec),
            ("stateful-exec", RiskClass::StatefulExec),
            ("destructive", RiskClass::Destructive),
            ("external-side-effect", RiskClass::ExternalSideEffect),
        ];

        for (kebab_name, expected) in cases {
            let parsed = match kebab_name {
                "safe-read" => RiskClass::SafeRead,
                "stateful-exec" => RiskClass::StatefulExec,
                "destructive" => RiskClass::Destructive,
                "external-side-effect" => RiskClass::ExternalSideEffect,
                _ => RiskClass::SafeExec,
            };
            assert_eq!(
                parsed, expected,
                "RiskClass variant mismatch for '{}'",
                kebab_name
            );
        }
    }
}
