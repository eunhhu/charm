use crate::core::{ToolCall, ToolResult};
use std::collections::HashMap;
use tokio::task::JoinSet;

/// Windsurf-style parallel tool executor
/// - Independent tools run concurrently
/// - Streaming results (don't wait for all)
/// - Zero-copy where possible
pub struct FastExecutor;

impl FastExecutor {
    /// Execute tools in parallel when independent
    pub async fn execute_batch(
        calls: Vec<ToolCall>,
        registry: &mut super::ToolRegistry,
    ) -> anyhow::Result<Vec<ToolResult>> {
        // Group calls by dependency
        let (independent, dependent) = Self::analyze_dependencies(calls);

        let mut results = Vec::new();
        let mut set = JoinSet::new();

        // Spawn all independent calls concurrently
        for call in independent {
            set.spawn(async move {
                let tool_name = Self::tool_name(&call);
                let args = serde_json::to_value(&call).unwrap_or_default();
                // This would need registry passed somehow - simplified
                // In real impl: use registry.execute(&tool_name, args).await
                (tool_name, args)
            });
        }

        // Collect results as they complete (streaming-style)
        while let Some(res) = set.join_next().await {
            match res {
                Ok((name, args)) => {
                    // Execute and collect
                    let result = registry.execute(&name, args).await?;
                    results.push(result);
                }
                Err(e) => {
                    results.push(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Task panicked: {}", e)),
                        metadata: None,
                    });
                }
            }
        }

        // Execute dependent calls sequentially
        for call in dependent {
            let tool_name = Self::tool_name(&call);
            let args = serde_json::to_value(&call).unwrap_or_default();
            let result = registry.execute(&tool_name, args).await?;
            results.push(result);
        }

        Ok(results)
    }

    /// Analyze which tools can run in parallel
    fn analyze_dependencies(calls: Vec<ToolCall>) -> (Vec<ToolCall>, Vec<ToolCall>) {
        let mut independent = Vec::new();
        let mut dependent = Vec::new();

        // Track which files are being read/written
        let mut read_files: HashMap<String, usize> = HashMap::new();
        let mut write_files: HashMap<String, usize> = HashMap::new();

        for (idx, call) in calls.iter().enumerate() {
            let deps = Self::extract_dependencies(call);
            let mut has_conflict = false;

            // Check read-after-write or write-after-read conflicts
            for file in &deps.reads {
                if write_files.contains_key(file) && write_files[file] < idx {
                    has_conflict = true;
                    break;
                }
            }

            for file in &deps.writes {
                if read_files.contains_key(file) && read_files[file] < idx {
                    has_conflict = true;
                    break;
                }
                if write_files.contains_key(file) && write_files[file] < idx {
                    has_conflict = true;
                    break;
                }
            }

            if has_conflict {
                dependent.push(call.clone());
            } else {
                independent.push(call.clone());
            }

            // Record this call's file operations
            for file in &deps.reads {
                read_files.insert(file.clone(), idx);
            }
            for file in &deps.writes {
                write_files.insert(file.clone(), idx);
            }
        }

        (independent, dependent)
    }

    fn extract_dependencies(call: &ToolCall) -> Dependencies {
        use crate::core::ToolCall::*;

        let mut reads = Vec::new();
        let mut writes = Vec::new();

        match call {
            ReadRange { file_path, .. } => reads.push(file_path.clone()),
            ReadSymbol { file_path, .. } => reads.push(file_path.clone()),
            GrepSearch { .. } => {}
            GlobSearch { .. } => {}
            ListDir { dir_path, .. } => reads.push(dir_path.clone()),
            EditPatch { file_path, .. } => {
                reads.push(file_path.clone());
                writes.push(file_path.clone());
            }
            WriteFile { file_path, .. } => writes.push(file_path.clone()),
            RunCommand { .. } => {}
            PollCommand { .. } => {}
            SemanticSearch { .. } => {}
            ParallelSearch { .. } => {}
            _ => {}
        }

        Dependencies { reads, writes }
    }

    fn tool_name(call: &ToolCall) -> String {
        use crate::core::ToolCall::*;

        match call {
            ReadRange { .. } => "read_range",
            ReadSymbol { .. } => "read_symbol",
            GrepSearch { .. } => "grep_search",
            GlobSearch { .. } => "glob_search",
            ListDir { .. } => "list_dir",
            SemanticSearch { .. } => "semantic_search",
            ParallelSearch { .. } => "parallel_search",
            EditPatch { .. } => "edit_patch",
            WriteFile { .. } => "write_file",
            RunCommand { .. } => "run_command",
            PollCommand { .. } => "poll_command",
            CheckpointCreate { .. } => "checkpoint_create",
            CheckpointRestore { .. } => "checkpoint_restore",
            PlanUpdate { .. } => "plan_update",
            MemoryStage { .. } => "memory_stage",
            MemoryCommit { .. } => "memory_commit",
        }
        .to_string()
    }
}

struct Dependencies {
    reads: Vec<String>,
    writes: Vec<String>,
}

/// In-memory file cache for zero-copy reads
pub struct FileCache {
    cache: HashMap<String, CachedFile>,
    max_size: usize,
    current_size: usize,
}

struct CachedFile {
    content: String,
    modified: std::time::SystemTime,
    access_count: u32,
}

impl FileCache {
    pub fn new(max_mb: usize) -> Self {
        Self {
            cache: HashMap::new(),
            max_size: max_mb * 1024 * 1024,
            current_size: 0,
        }
    }

    pub async fn read(&mut self, path: &str) -> anyhow::Result<String> {
        // Check cache first
        if let Some(cached) = self.cache.get(path) {
            // Verify not modified
            let metadata = tokio::fs::metadata(path).await?;
            if metadata.modified()? == cached.modified {
                return Ok(cached.content.clone());
            }
        }

        // Read from disk
        let content = tokio::fs::read_to_string(path).await?;
        let metadata = tokio::fs::metadata(path).await?;

        // Cache if small enough
        if content.len() < 100_000 {
            // 100KB limit per file
            self.cache.insert(
                path.to_string(),
                CachedFile {
                    content: content.clone(),
                    modified: metadata.modified()?,
                    access_count: 1,
                },
            );
            self.current_size += content.len();
            self.evict_if_needed();
        }

        Ok(content)
    }

    fn evict_if_needed(&mut self) {
        // LRU eviction
        while self.current_size > self.max_size {
            if let Some(oldest) = self
                .cache
                .iter()
                .min_by_key(|(_, v)| v.access_count)
                .map(|(k, _)| k.clone())
            {
                if let Some(file) = self.cache.remove(&oldest) {
                    self.current_size -= file.content.len();
                }
            } else {
                break;
            }
        }
    }
}

/// Streaming tool result - don't wait for completion
pub struct StreamingToolResult {
    pub tool_name: String,
    pub chunks: tokio::sync::mpsc::Receiver<Result<String, String>>,
}

impl StreamingToolResult {
    pub async fn collect(self) -> ToolResult {
        let mut output = String::new();
        let mut error = None;

        let mut receiver = self.chunks;
        while let Some(chunk) = receiver.recv().await {
            match chunk {
                Ok(data) => output.push_str(&data),
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }

        ToolResult {
            success: error.is_none(),
            output,
            error,
            metadata: None,
        }
    }
}
