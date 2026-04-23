use crate::core::ToolResult;
use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TodoStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "in_progress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
}

impl Default for TodoStatus {
    fn default() -> Self {
        TodoStatus::Pending
    }
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TodoStatus::Pending => write!(f, "pending"),
            TodoStatus::InProgress => write!(f, "in_progress"),
            TodoStatus::Completed => write!(f, "completed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
    pub priority: String, // high, medium, low
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TodoList {
    pub items: Vec<TodoItem>,
}

/// Global todo storage per workspace
static TODO_STORAGE: tokio::sync::OnceCell<Arc<Mutex<HashMap<std::path::PathBuf, TodoList>>>> =
    tokio::sync::OnceCell::const_new();

async fn get_storage() -> Arc<Mutex<HashMap<std::path::PathBuf, TodoList>>> {
    TODO_STORAGE
        .get_or_init(|| async { Arc::new(Mutex::new(HashMap::new())) })
        .await
        .clone()
}

async fn get_or_create_todo_list(cwd: &Path) -> TodoList {
    let storage = get_storage().await;
    let guard = storage.lock().await;
    guard.get(cwd).cloned().unwrap_or_default()
}

async fn save_todo_list(cwd: &Path, list: TodoList) {
    let storage = get_storage().await;
    let mut guard = storage.lock().await;
    guard.insert(cwd.to_path_buf(), list);
}

pub async fn todo_add(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .context("Missing 'content' parameter")?;

    let priority = args
        .get("priority")
        .and_then(|v| v.as_str())
        .unwrap_or("medium");

    let mut list = get_or_create_todo_list(cwd).await;

    let todo = TodoItem {
        id: Uuid::new_v4().to_string(),
        content: content.to_string(),
        status: TodoStatus::Pending,
        priority: priority.to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let id = todo.id.clone();
    list.items.push(todo);
    save_todo_list(cwd, list).await;

    Ok(ToolResult {
        success: true,
        output: format!("Added todo: {} (id: {})", content, &id[..8]),
        error: None,
        metadata: Some(serde_json::json!({
            "id": id
        })),
    })
}

pub async fn todo_list_items(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let status_filter = args.get("status").and_then(|v| v.as_str());
    let list = get_or_create_todo_list(cwd).await;

    if list.items.is_empty() {
        return Ok(ToolResult {
            success: true,
            output: "No todos found.".to_string(),
            error: None,
            metadata: None,
        });
    }

    let filtered: Vec<TodoItem> = list
        .items
        .iter()
        .filter(|item| {
            if let Some(status) = status_filter {
                item.status.to_string() == status
            } else {
                true
            }
        })
        .cloned()
        .collect();

    if filtered.is_empty() {
        return Ok(ToolResult {
            success: true,
            output: format!("No todos with status '{}'.", status_filter.unwrap_or("")),
            error: None,
            metadata: None,
        });
    }

    let mut output = String::new();
    output.push_str(&format!("Found {} todos:\n\n", filtered.len()));

    let count = filtered.len();
    for item in filtered {
        let icon = match item.status {
            TodoStatus::Pending => "○",
            TodoStatus::InProgress => "◐",
            TodoStatus::Completed => "✓",
        };
        let priority_icon = match item.priority.as_str() {
            "high" => "🔴",
            "medium" => "🟡",
            "low" => "🟢",
            _ => "⚪",
        };
        output.push_str(&format!(
            "{} {} [{}] {} ({}): {}\n",
            icon,
            priority_icon,
            &item.id[..8],
            item.status,
            item.priority,
            item.content
        ));
    }

    Ok(ToolResult {
        success: true,
        output,
        error: None,
        metadata: Some(serde_json::json!({
            "count": count
        })),
    })
}

pub async fn todo_update(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' parameter")?;

    let mut list = get_or_create_todo_list(cwd).await;

    let item = list.items.iter_mut().find(|item| item.id.starts_with(id));

    match item {
        Some(item) => {
            if let Some(content) = args.get("content").and_then(|v| v.as_str()) {
                item.content = content.to_string();
            }

            if let Some(status) = args.get("status").and_then(|v| v.as_str()) {
                item.status = match status {
                    "pending" => TodoStatus::Pending,
                    "in_progress" => TodoStatus::InProgress,
                    "completed" => TodoStatus::Completed,
                    _ => item.status.clone(),
                };
            }

            if let Some(priority) = args.get("priority").and_then(|v| v.as_str()) {
                item.priority = priority.to_string();
            }

            item.updated_at = Utc::now();
            let content = item.content.clone();
            let status = item.status.clone();

            save_todo_list(cwd, list).await;

            Ok(ToolResult {
                success: true,
                output: format!("Updated todo: {} (status: {})", content, status),
                error: None,
                metadata: None,
            })
        }
        None => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("Todo with id '{}' not found.", id)),
            metadata: None,
        }),
    }
}

pub async fn todo_delete(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' parameter")?;

    let mut list = get_or_create_todo_list(cwd).await;
    let original_len = list.items.len();

    list.items.retain(|item| !item.id.starts_with(id));

    if list.items.len() == original_len {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("Todo with id '{}' not found.", id)),
            metadata: None,
        });
    }

    save_todo_list(cwd, list).await;

    Ok(ToolResult {
        success: true,
        output: format!("Deleted todo: {}", id),
        error: None,
        metadata: None,
    })
}

pub async fn todo_clear(_args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    save_todo_list(cwd, TodoList::default()).await;

    Ok(ToolResult {
        success: true,
        output: "Cleared all todos.".to_string(),
        error: None,
        metadata: None,
    })
}
