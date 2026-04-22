use crate::core::ToolResult;
use serde_json::Value;
use std::path::Path;

mod cache;
mod command;
mod file;
mod memory_tools;
mod patch;
mod retrieval;
pub mod rtk_filter;
pub mod search;
mod semantic;

use cache::ToolCache;

pub struct ToolRegistry {
    cwd: std::path::PathBuf,
    cache: ToolCache,
}

impl ToolRegistry {
    pub fn new(cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            cache: ToolCache::new(50),
        }
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

    async fn execute_uncached(&self, tool: &str, args: Value) -> anyhow::Result<ToolResult> {
        let hint = args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or(tool)
            .to_string();
        let mut result = match tool {
            "read_range" => file::read_range(args, &self.cwd).await,
            "write_file" => file::write_file(args, &self.cwd).await,
            "grep_search" => search::grep_search(args, &self.cwd).await,
            "glob_search" => search::glob_search(args, &self.cwd).await,
            "list_dir" => search::list_dir(args).await,
            "edit_patch" => patch::edit_patch(args).await,
            "semantic_search" => semantic::semantic_search(args, &self.cwd).await,
            "parallel_search" => retrieval::parallel_search(args, &self.cwd).await,
            "run_command" => command::run_command(args, &self.cwd).await,
            "poll_command" => command::poll_command(args).await,
            "checkpoint_create" => {
                let cm = crate::harness::CheckpointManager::new(&self.cwd)?;
                cm.create(args)
            }
            "checkpoint_restore" => {
                let cm = crate::harness::CheckpointManager::new(&self.cwd)?;
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
            "run_command" | "poll_command" => {
                let filtered = rtk_filter::filter_with_rtk(&result.output, &hint).await;
                if filtered.len() < result.output.len() {
                    result.output = filtered;
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
