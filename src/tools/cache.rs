use crate::core::ToolResult;
use serde_json::Value;
use std::collections::HashMap;

pub struct ToolCache {
    capacity: usize,
    order: Vec<String>,
    store: HashMap<String, ToolResult>,
}

impl ToolCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            order: Vec::with_capacity(capacity),
            store: HashMap::with_capacity(capacity),
        }
    }

    pub fn get(&mut self, tool: &str, args: &Value) -> Option<ToolResult> {
        let key = cache_key(tool, args);
        if self.store.contains_key(&key) {
            self.order.retain(|k| k != &key);
            self.order.push(key.clone());
            self.store.get(&key).cloned()
        } else {
            None
        }
    }

    pub fn put(&mut self, tool: &str, args: &Value, result: ToolResult) {
        let key = cache_key(tool, args);
        if self.store.contains_key(&key) {
            self.order.retain(|k| k != &key);
        } else if self.order.len() >= self.capacity
            && let Some(oldest) = self.order.first().cloned()
        {
            self.order.remove(0);
            self.store.remove(&oldest);
        }
        self.order.push(key.clone());
        self.store.insert(key, result);
    }

    pub fn is_cachable(tool: &str) -> bool {
        matches!(
            tool,
            "grep_search" | "glob_search" | "semantic_search" | "parallel_search" | "list_dir"
        )
    }
}

fn cache_key(tool: &str, args: &Value) -> String {
    format!("{}:{}", tool, args)
}
