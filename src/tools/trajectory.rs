use crate::core::ToolResult;
use anyhow::Context;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

/// In-memory storage for session trajectories
static TRAJECTORY_STORAGE: tokio::sync::OnceCell<Arc<Mutex<Vec<TrajectoryEntry>>>> =
    tokio::sync::OnceCell::const_new();

#[derive(Debug, Clone)]
pub struct TrajectoryEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub role: String,
    pub content: String,
    pub tool_calls: Option<Vec<serde_json::Value>>,
    pub tool_results: Option<Vec<serde_json::Value>>,
}

async fn get_storage() -> Arc<Mutex<Vec<TrajectoryEntry>>> {
    TRAJECTORY_STORAGE
        .get_or_init(|| async { Arc::new(Mutex::new(Vec::new())) })
        .await
        .clone()
}

pub async fn store_trajectory_entry(
    role: &str,
    content: &str,
    tool_calls: Option<Vec<serde_json::Value>>,
    tool_results: Option<Vec<serde_json::Value>>,
) {
    let storage = get_storage().await;
    let mut guard = storage.lock().await;
    guard.push(TrajectoryEntry {
        timestamp: chrono::Utc::now(),
        role: role.to_string(),
        content: content.to_string(),
        tool_calls,
        tool_results,
    });
}

pub async fn trajectory_search(args: Value) -> anyhow::Result<ToolResult> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .context("Missing 'query' parameter")?;

    let case_sensitive = args
        .get("case_sensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    let storage = get_storage().await;
    let guard = storage.lock().await;

    let search_fn = if case_sensitive {
        |text: &str, q: &str| text.contains(q)
    } else {
        |text: &str, q: &str| text.to_lowercase().contains(&q.to_lowercase())
    };

    let matches: Vec<&TrajectoryEntry> = guard
        .iter()
        .filter(|entry| search_fn(&entry.content, query))
        .take(limit)
        .collect();

    if matches.is_empty() {
        return Ok(ToolResult {
            success: true,
            output: format!("No matches found for: {}", query),
            error: None,
            metadata: Some(serde_json::json!({
                "query": query,
                "matches": 0
            })),
        });
    }

    let mut output = format!("Found {} matches for '{}'\n\n", matches.len(), query);

    for (idx, entry) in matches.iter().enumerate() {
        output.push_str(&format!(
            "[{}] {} at {}\n",
            idx + 1,
            entry.role,
            entry.timestamp.format("%Y-%m-%d %H:%M:%S")
        ));

        let preview = if entry.content.len() > 500 {
            format!("{}...", &entry.content[..500])
        } else {
            entry.content.clone()
        };
        output.push_str(&format!("{}", preview));

        if entry.tool_calls.is_some() || entry.tool_results.is_some() {
            output.push_str("\n[contains tool interactions]");
        }
        output.push_str("\n\n---\n\n");
    }

    Ok(ToolResult {
        success: true,
        output,
        error: None,
        metadata: Some(serde_json::json!({
            "query": query,
            "matches": matches.len()
        })),
    })
}

pub async fn trajectory_list(args: Value) -> anyhow::Result<ToolResult> {
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

    let storage = get_storage().await;
    let guard = storage.lock().await;

    if guard.is_empty() {
        return Ok(ToolResult {
            success: true,
            output: "No trajectory entries found.".to_string(),
            error: None,
            metadata: Some(serde_json::json!({
                "total": 0
            })),
        });
    }

    let entries: Vec<&TrajectoryEntry> = guard.iter().rev().take(limit).collect();

    let mut output = format!(
        "Showing {} recent entries (total: {}):\n\n",
        entries.len(),
        guard.len()
    );

    for (idx, entry) in entries.iter().enumerate() {
        let content_preview: String = entry.content.chars().take(100).collect();
        output.push_str(&format!(
            "[{}] {} at {}\n{}",
            guard.len() - idx,
            entry.role,
            entry.timestamp.format("%H:%M:%S"),
            if entry.content.len() > 100 {
                format!("{}...\n", content_preview)
            } else {
                format!("{}\n", content_preview)
            }
        ));
        if entry.tool_calls.is_some() {
            output.push_str("  [tool calls]\n");
        }
        output.push('\n');
    }

    Ok(ToolResult {
        success: true,
        output,
        error: None,
        metadata: Some(serde_json::json!({
            "total": guard.len(),
            "shown": entries.len()
        })),
    })
}

pub async fn trajectory_clear(_args: Value) -> anyhow::Result<ToolResult> {
    let storage = get_storage().await;
    let mut guard = storage.lock().await;
    let count = guard.len();
    guard.clear();

    Ok(ToolResult {
        success: true,
        output: format!("Cleared {} trajectory entries.", count),
        error: None,
        metadata: Some(serde_json::json!({
            "cleared": count
        })),
    })
}

pub async fn trajectory_get_context(args: Value) -> anyhow::Result<ToolResult> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .context("Missing 'query' parameter")?;

    let before = args.get("before").and_then(|v| v.as_u64()).unwrap_or(2) as usize;

    let after = args.get("after").and_then(|v| v.as_u64()).unwrap_or(2) as usize;

    let storage = get_storage().await;
    let guard = storage.lock().await;

    // Find matching entry index
    let match_idx = guard
        .iter()
        .position(|entry| entry.content.to_lowercase().contains(&query.to_lowercase()));

    let idx = match match_idx {
        Some(i) => i,
        None => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("No match found for: {}", query)),
                metadata: None,
            });
        }
    };

    let start = idx.saturating_sub(before);
    let end = (idx + after + 1).min(guard.len());
    let context = &guard[start..end];

    let mut output = format!("Context around match #{}:\n\n", idx + 1);
    for (i, entry) in context.iter().enumerate() {
        let marker = if start + i == idx { ">>> " } else { "    " };
        let content = if entry.content.len() > 300 {
            format!("{}...", &entry.content[..300])
        } else {
            entry.content.clone()
        };
        output.push_str(&format!(
            "{}[{}] {}: {}\n",
            marker,
            start + i + 1,
            entry.role,
            content
        ));
    }

    Ok(ToolResult {
        success: true,
        output,
        error: None,
        metadata: Some(serde_json::json!({
            "match_index": idx,
            "context_range": [start, end]
        })),
    })
}
