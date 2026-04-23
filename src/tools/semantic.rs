use crate::core::ToolResult;
use crate::indexer::store::IndexStore;
use serde_json::Value;

pub async fn semantic_search(args: Value, cwd: &std::path::Path) -> anyhow::Result<ToolResult> {
    let query = args["query"].as_str().unwrap_or("");
    let top_k = args["top_k"].as_u64().map(|v| v as usize).unwrap_or(10);
    let expand = args["expand_full"].as_bool().unwrap_or(false);

    if query.is_empty() {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("query cannot be empty".to_string()),
            metadata: None,
        });
    }

    let store = IndexStore::new(cwd);
    let index = store.load()?;

    if index.symbols.is_empty() {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Index is empty. Run 'charm index' first.".to_string()),
            metadata: None,
        });
    }

    let results = index.search(query, top_k * 2);

    let mut output_lines = Vec::new();
    for (i, sym) in results.iter().take(top_k).enumerate() {
        if expand || i < 3 {
            output_lines.push(format!(
                "[{}] {} {} ({}:{}-{})\n{}",
                sym.kind,
                sym.name,
                sym.file_path,
                sym.line,
                sym.body_start,
                sym.body_end,
                sym.signature
            ));
            if let Some(doc) = &sym.docstring {
                output_lines.push(format!("  doc: {}", doc));
            }
        } else {
            output_lines.push(format!(
                "[{}] {} ({}:{})  {}",
                sym.kind, sym.name, sym.file_path, sym.line, sym.signature
            ));
        }
    }

    Ok(ToolResult {
        success: true,
        output: output_lines.join("\n"),
        error: None,
        metadata: Some(serde_json::json!({
            "query": query,
            "results_count": results.len().min(top_k),
            "total_indexed": index.symbols.len()
        })),
    })
}
