use crate::core::ToolResult;
use crate::retrieval::worker::RetrievalWorker;
use serde_json::Value;

pub async fn parallel_search(args: Value, cwd: &std::path::Path) -> anyhow::Result<ToolResult> {
    let query = args["query"].as_str().unwrap_or("");
    let top_k = args["top_k"].as_u64().map(|v| v as usize).unwrap_or(10);

    if query.is_empty() {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("query cannot be empty".to_string()),
            metadata: None,
        });
    }

    let worker = RetrievalWorker::new(cwd);
    let result = worker.retrieve(query, top_k).await?;

    let mut output_lines = vec![result.summary];
    output_lines.push(String::new());

    for (i, ev) in result.evidence.iter().enumerate() {
        output_lines.push(format!("{}. [{}] {}", i + 1, ev.source, ev.file_path));
        output_lines.push(format!("   {}", ev.snippet.lines().next().unwrap_or("")));
    }

    Ok(ToolResult {
        success: true,
        output: output_lines.join("\n"),
        error: None,
        metadata: Some(serde_json::json!({
            "query": result.query,
            "matches": result.evidence.len()
        })),
    })
}
