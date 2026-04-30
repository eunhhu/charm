use crate::core::ToolResult;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JupyterNotebook {
    pub cells: Vec<JupyterCell>,
    pub metadata: serde_json::Value,
    pub nbformat: i32,
    pub nbformat_minor: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JupyterCell {
    #[serde(rename = "cell_type")]
    pub cell_type: String,
    pub source: Vec<String>,
    pub metadata: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<JupyterOutput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_count: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JupyterOutput {
    #[serde(rename = "output_type")]
    pub output_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

pub async fn read_notebook(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .context("Missing 'path' parameter")?;

    let full_path = cwd.join(path);
    let content = tokio::fs::read_to_string(&full_path)
        .await
        .context(format!("Failed to read notebook: {}", full_path.display()))?;

    let notebook: JupyterNotebook =
        serde_json::from_str(&content).context("Failed to parse notebook JSON")?;

    // Extract cell info for display
    let mut output = format!(
        "Notebook: {}\nFormat: {}.{}, Cells: {}\n\n",
        full_path.display(),
        notebook.nbformat,
        notebook.nbformat_minor,
        notebook.cells.len()
    );

    for (idx, cell) in notebook.cells.iter().enumerate() {
        let source = cell.source.join("");
        let preview: String = source.chars().take(200).collect();
        output.push_str(&format!(
            "[{}] {} ({} lines)\n{}\n",
            idx,
            cell.cell_type.to_uppercase(),
            cell.source.len(),
            if source.len() > 200 {
                format!("{}...", preview)
            } else {
                preview
            }
        ));
        if idx < notebook.cells.len() - 1 {
            output.push('\n');
        }
    }

    Ok(ToolResult {
        success: true,
        output,
        error: None,
        metadata: Some(serde_json::json!({
            "path": full_path.to_string_lossy().to_string(),
            "cell_count": notebook.cells.len()
        })),
    })
}

pub async fn read_notebook_cell(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .context("Missing 'path' parameter")?;

    let cell_index = args
        .get("cell_index")
        .and_then(|v| v.as_u64())
        .context("Missing 'cell_index' parameter")? as usize;

    let full_path = cwd.join(path);
    let content = tokio::fs::read_to_string(&full_path)
        .await
        .context(format!("Failed to read notebook: {}", full_path.display()))?;

    let notebook: JupyterNotebook =
        serde_json::from_str(&content).context("Failed to parse notebook JSON")?;

    let cell = notebook
        .cells
        .get(cell_index)
        .context(format!("Cell index {} not found", cell_index))?;

    let source = cell.source.join("");
    let mut output = format!(
        "Cell [{}] - {} (execution_count: {:?})\n\n{}",
        cell_index, cell.cell_type, cell.execution_count, source
    );

    if let Some(ref outputs) = cell.outputs
        && !outputs.is_empty()
    {
        output.push_str("\n\n--- Output ---\n");
        for out in outputs {
            if let Some(ref text) = out.text {
                output.push_str(&text.join(""));
            }
        }
    }

    Ok(ToolResult {
        success: true,
        output,
        error: None,
        metadata: Some(serde_json::json!({
            "cell_type": cell.cell_type,
            "cell_index": cell_index
        })),
    })
}

pub async fn edit_notebook_cell(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .context("Missing 'path' parameter")?;

    let cell_index = args
        .get("cell_index")
        .and_then(|v| v.as_u64())
        .context("Missing 'cell_index' parameter")? as usize;

    let new_source = args
        .get("source")
        .and_then(|v| v.as_str())
        .context("Missing 'source' parameter")?;

    let full_path = cwd.join(path);
    let content = tokio::fs::read_to_string(&full_path)
        .await
        .context(format!("Failed to read notebook: {}", full_path.display()))?;

    let mut notebook: JupyterNotebook =
        serde_json::from_str(&content).context("Failed to parse notebook JSON")?;

    let cell = notebook
        .cells
        .get_mut(cell_index)
        .context(format!("Cell index {} not found", cell_index))?;

    cell.source = new_source.lines().map(|s| s.to_string()).collect();

    let updated = serde_json::to_string_pretty(&notebook)?;
    tokio::fs::write(&full_path, updated)
        .await
        .context("Failed to write notebook")?;

    Ok(ToolResult {
        success: true,
        output: format!("Updated cell {} in {}", cell_index, full_path.display()),
        error: None,
        metadata: Some(serde_json::json!({
            "cell_index": cell_index
        })),
    })
}

pub async fn insert_notebook_cell(args: Value, cwd: &Path) -> anyhow::Result<ToolResult> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .context("Missing 'path' parameter")?;

    let cell_type = args
        .get("cell_type")
        .and_then(|v| v.as_str())
        .unwrap_or("code");

    let source = args.get("source").and_then(|v| v.as_str()).unwrap_or("");

    let insert_after = args
        .get("insert_after")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);

    let full_path = cwd.join(path);
    let content = tokio::fs::read_to_string(&full_path)
        .await
        .context(format!("Failed to read notebook: {}", full_path.display()))?;

    let mut notebook: JupyterNotebook =
        serde_json::from_str(&content).context("Failed to parse notebook JSON")?;

    let new_cell = JupyterCell {
        cell_type: cell_type.to_string(),
        source: source.lines().map(|s| s.to_string()).collect(),
        metadata: serde_json::Value::Object(serde_json::Map::new()),
        outputs: if cell_type == "code" {
            Some(vec![])
        } else {
            None
        },
        execution_count: None,
    };

    if let Some(idx) = insert_after {
        if idx < notebook.cells.len() {
            notebook.cells.insert(idx + 1, new_cell);
        } else {
            notebook.cells.push(new_cell);
        }
    } else {
        notebook.cells.push(new_cell);
    }

    let updated = serde_json::to_string_pretty(&notebook)?;
    tokio::fs::write(&full_path, updated)
        .await
        .context("Failed to write notebook")?;

    Ok(ToolResult {
        success: true,
        output: format!("Inserted new {} cell in {}", cell_type, full_path.display()),
        error: None,
        metadata: None,
    })
}
