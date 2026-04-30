use crate::core::{ToolCall, ToolResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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
        let cwd = registry.cwd().to_path_buf();
        // Group calls by dependency
        let (independent, dependent) = Self::analyze_dependencies(calls);

        let mut results: Vec<Option<ToolResult>> = Vec::new();
        let total_len = independent.len() + dependent.len();
        results.resize_with(total_len, || None);
        let mut set = JoinSet::new();

        // Spawn all independent calls concurrently
        for (idx, call) in independent.into_iter().enumerate() {
            let cwd = cwd.clone();
            set.spawn(async move {
                let tool_name = Self::tool_name(&call);
                let args = serde_json::to_value(&call).unwrap_or_default();
                let mut registry = super::ToolRegistry::new(&cwd);
                let result = registry.execute(&tool_name, args).await;
                (idx, result)
            });
        }

        // Collect results as they complete (streaming-style)
        while let Some(res) = set.join_next().await {
            match res {
                Ok((idx, Ok(result))) => {
                    results[idx] = Some(result);
                }
                Ok((idx, Err(e))) => {
                    results[idx] = Some(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Tool execution failed: {e}")),
                        metadata: None,
                    });
                }
                Err(e) => {
                    results.push(Some(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Task panicked: {}", e)),
                        metadata: None,
                    }));
                }
            }
        }

        // Execute dependent calls sequentially
        let dependent_offset = results.iter().filter(|item| item.is_some()).count();
        for (offset, call) in dependent.into_iter().enumerate() {
            let tool_name = Self::tool_name(&call);
            let args = serde_json::to_value(&call).unwrap_or_default();
            let result = match registry.execute(&tool_name, args).await {
                Ok(result) => result,
                Err(e) => ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Tool execution failed: {e}")),
                    metadata: None,
                },
            };
            let idx = dependent_offset + offset;
            if idx < results.len() {
                results[idx] = Some(result);
            } else {
                results.push(Some(result));
            }
        }

        Ok(results.into_iter().flatten().collect())
    }

    /// Analyze which tools can run in parallel
    fn analyze_dependencies(calls: Vec<ToolCall>) -> (Vec<ToolCall>, Vec<ToolCall>) {
        let mut independent = Vec::new();
        let mut dependent = Vec::new();

        // Track which files are being read/written. Only read-only calls can
        // run through independent registries; mutating and shell tools stay
        // ordered to avoid hidden side effects.
        let mut read_files: HashMap<String, usize> = HashMap::new();
        let mut write_files: HashMap<String, usize> = HashMap::new();
        let mut ordered_tail = false;

        for (idx, call) in calls.iter().enumerate() {
            if ordered_tail || !Self::is_parallel_safe(call) {
                dependent.push(call.clone());
                ordered_tail = true;
                continue;
            }

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

    fn is_parallel_safe(call: &ToolCall) -> bool {
        matches!(
            call,
            ToolCall::ReadRange { .. }
                | ToolCall::ReadSymbol { .. }
                | ToolCall::GrepSearch { .. }
                | ToolCall::GlobSearch { .. }
                | ToolCall::ListDir { .. }
                | ToolCall::SemanticSearch { .. }
                | ToolCall::ParallelSearch { .. }
        )
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
            CancelCommand { .. } => {}
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
            CancelCommand { .. } => "cancel_command",
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
    persist_path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct PersistedFileCache {
    entries: HashMap<String, PersistedCachedFile>,
}

#[derive(Serialize, Deserialize)]
struct PersistedCachedFile {
    content: String,
    modified_unix_nanos: u128,
    len: u64,
    access_count: u32,
}

struct CachedFile {
    content: String,
    modified: std::time::SystemTime,
    len: u64,
    access_count: u32,
}

pub struct CachedRead {
    pub content: String,
    pub cache_hit: bool,
}

impl FileCache {
    #[cfg(test)]
    pub fn new(max_mb: usize) -> Self {
        Self {
            cache: HashMap::new(),
            max_size: max_mb * 1024 * 1024,
            current_size: 0,
            persist_path: None,
        }
    }

    pub fn with_persistence(max_mb: usize, persist_path: PathBuf) -> Self {
        let mut cache = Self {
            cache: HashMap::new(),
            max_size: max_mb * 1024 * 1024,
            current_size: 0,
            persist_path: Some(persist_path),
        };
        cache.load_persisted();
        cache
    }

    pub async fn read(&mut self, path: &Path) -> anyhow::Result<CachedRead> {
        let key = path.to_string_lossy().to_string();
        let metadata = tokio::fs::metadata(path).await?;
        let modified = metadata.modified()?;

        if let Some(cached) = self.cache.get_mut(&key) {
            if modified == cached.modified && metadata.len() == cached.len {
                cached.access_count = cached.access_count.saturating_add(1);
                return Ok(CachedRead {
                    content: cached.content.clone(),
                    cache_hit: true,
                });
            }
        }
        self.invalidate(path);

        // Read from disk
        let content = tokio::fs::read_to_string(path).await?;

        // Cache if small enough
        if content.len() < 100_000 && content.len() <= self.max_size {
            // 100KB limit per file
            self.cache.insert(
                key,
                CachedFile {
                    content: content.clone(),
                    modified,
                    len: metadata.len(),
                    access_count: 1,
                },
            );
            self.current_size += content.len();
            self.evict_if_needed();
            self.persist();
        }

        Ok(CachedRead {
            content,
            cache_hit: false,
        })
    }

    pub fn invalidate(&mut self, path: &Path) {
        let key = path.to_string_lossy().to_string();
        if let Some(file) = self.cache.remove(&key) {
            self.current_size = self.current_size.saturating_sub(file.content.len());
            self.persist();
        }
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
                    self.persist();
                }
            } else {
                break;
            }
        }
    }

    fn load_persisted(&mut self) {
        let Some(path) = self.persist_path.as_ref() else {
            return;
        };
        let Ok(raw) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(persisted) = serde_json::from_str::<PersistedFileCache>(&raw) else {
            return;
        };
        for (key, file) in persisted.entries {
            if file.content.len() > self.max_size {
                continue;
            }
            self.current_size += file.content.len();
            self.cache.insert(
                key,
                CachedFile {
                    content: file.content,
                    modified: system_time_from_unix_nanos(file.modified_unix_nanos),
                    len: file.len,
                    access_count: file.access_count,
                },
            );
        }
        self.evict_if_needed();
    }

    fn persist(&self) {
        let Some(path) = self.persist_path.as_ref() else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let entries = self
            .cache
            .iter()
            .map(|(key, file)| {
                (
                    key.clone(),
                    PersistedCachedFile {
                        content: file.content.clone(),
                        modified_unix_nanos: unix_nanos(file.modified),
                        len: file.len,
                        access_count: file.access_count,
                    },
                )
            })
            .collect();
        let payload = PersistedFileCache { entries };
        if let Ok(raw) = serde_json::to_string(&payload) {
            let _ = std::fs::write(path, raw);
        }
    }
}

fn unix_nanos(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn system_time_from_unix_nanos(nanos: u128) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(nanos.min(u64::MAX as u128) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn file_cache_reuses_unchanged_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        tokio::fs::write(&path, "alpha\nbeta\n").await.unwrap();

        let mut cache = FileCache::new(1);
        let first = cache.read(&path).await.unwrap();
        let second = cache.read(&path).await.unwrap();

        assert!(!first.cache_hit);
        assert!(second.cache_hit);
        let key = path.to_string_lossy().to_string();
        assert_eq!(cache.cache[&key].access_count, 2);
    }

    #[tokio::test]
    async fn file_cache_invalidates_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        tokio::fs::write(&path, "alpha").await.unwrap();

        let mut cache = FileCache::new(1);
        let _ = cache.read(&path).await.unwrap();
        assert_eq!(cache.cache.len(), 1);

        cache.invalidate(&path);
        assert!(cache.cache.is_empty());
        assert_eq!(cache.current_size, 0);
    }

    #[tokio::test]
    async fn file_cache_persists_unchanged_file_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        let cache_path = dir
            .path()
            .join(".charm")
            .join("cache")
            .join("file-cache.json");
        tokio::fs::write(&path, "alpha\nbeta\n").await.unwrap();

        let mut first_cache = FileCache::with_persistence(1, cache_path.clone());
        let first = first_cache.read(&path).await.unwrap();
        assert!(!first.cache_hit);

        let mut second_cache = FileCache::with_persistence(1, cache_path);
        let second = second_cache.read(&path).await.unwrap();
        assert!(second.cache_hit);
        assert_eq!(second.content, "alpha\nbeta\n");
    }

    #[test]
    fn dependency_analysis_keeps_order_after_mutating_call() {
        let calls = vec![
            ToolCall::WriteFile {
                file_path: "a.txt".to_string(),
                content: "new".to_string(),
            },
            ToolCall::ReadRange {
                file_path: "a.txt".to_string(),
                offset: None,
                limit: None,
            },
        ];

        let (independent, dependent) = FastExecutor::analyze_dependencies(calls);
        assert!(independent.is_empty());
        assert_eq!(dependent.len(), 2);
    }
}
