use crate::core::ToolResult;
use anyhow::Context;
use fantoccini::{Client, ClientBuilder, Locator};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Global browser client for reuse across tool calls
static BROWSER_CLIENT: tokio::sync::OnceCell<Arc<Mutex<Option<Client>>>> =
    tokio::sync::OnceCell::const_new();

async fn get_or_create_client() -> anyhow::Result<Arc<Mutex<Option<Client>>>> {
    let client = BROWSER_CLIENT
        .get_or_init(|| async { Arc::new(Mutex::new(None)) })
        .await;

    let mut guard = client.lock().await;
    if guard.is_none() {
        // Try to connect to existing WebDriver or start one
        let c = ClientBuilder::native()
            .connect("http://localhost:4444")
            .await
            .ok();
        *guard = c;
    }
    drop(guard);

    Ok(client.clone())
}

pub async fn browser_navigate(args: Value) -> anyhow::Result<ToolResult> {
    let url = args
        .get("url")
        .and_then(|v| v.as_str())
        .context("Missing 'url' parameter")?;

    let client = get_or_create_client().await?;
    let mut guard = client.lock().await;

    match guard.as_mut() {
        Some(c) => {
            c.goto(url).await.context("Failed to navigate")?;
            let current_url = c.current_url().await?;
            let title = c.title().await.unwrap_or_default();

            Ok(ToolResult {
                success: true,
                output: format!("Navigated to: {}\nTitle: {}", current_url, title),
                error: None,
                metadata: Some(serde_json::json!({
                    "url": current_url.to_string(),
                    "title": title
                })),
            })
        }
        None => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Browser automation not available. Chrome WebDriver must be running on localhost:4444".to_string()),
            metadata: None,
        }),
    }
}

pub async fn browser_screenshot(args: Value) -> anyhow::Result<ToolResult> {
    let filename = args
        .get("filename")
        .and_then(|v| v.as_str())
        .unwrap_or("screenshot.png");

    let client = get_or_create_client().await?;
    let mut guard = client.lock().await;

    match guard.as_mut() {
        Some(c) => {
            let screenshot = c.screenshot().await.context("Failed to take screenshot")?;
            tokio::fs::write(filename, screenshot).await.context("Failed to save screenshot")?;

            Ok(ToolResult {
                success: true,
                output: format!("Screenshot saved to: {}", filename),
                error: None,
                metadata: Some(serde_json::json!({
                    "filename": filename
                })),
            })
        }
        None => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Browser automation not available. Chrome WebDriver must be running on localhost:4444".to_string()),
            metadata: None,
        }),
    }
}

pub async fn browser_click(args: Value) -> anyhow::Result<ToolResult> {
    let selector = args
        .get("selector")
        .and_then(|v| v.as_str())
        .context("Missing 'selector' parameter (CSS selector)")?;

    let client = get_or_create_client().await?;
    let mut guard = client.lock().await;

    match guard.as_mut() {
        Some(c) => {
            let element = c
                .find(Locator::Css(selector))
                .await
                .context(format!("Element not found: {}", selector))?;
            element.click().await.context("Failed to click element")?;

            Ok(ToolResult {
                success: true,
                output: format!("Clicked element: {}", selector),
                error: None,
                metadata: None,
            })
        }
        None => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Browser automation not available".to_string()),
            metadata: None,
        }),
    }
}

pub async fn browser_type(args: Value) -> anyhow::Result<ToolResult> {
    let selector = args
        .get("selector")
        .and_then(|v| v.as_str())
        .context("Missing 'selector' parameter (CSS selector)")?;

    let text = args
        .get("text")
        .and_then(|v| v.as_str())
        .context("Missing 'text' parameter")?;

    let submit = args
        .get("submit")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let client = get_or_create_client().await?;
    let mut guard = client.lock().await;

    match guard.as_mut() {
        Some(c) => {
            let element = c
                .find(Locator::Css(selector))
                .await
                .context(format!("Element not found: {}", selector))?;

            if submit {
                element.send_keys(text).await?;
                element.send_keys("\n").await?;
            } else {
                element.send_keys(text).await?;
            }

            Ok(ToolResult {
                success: true,
                output: format!("Typed '{}' into element: {}", text, selector),
                error: None,
                metadata: None,
            })
        }
        None => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Browser automation not available".to_string()),
            metadata: None,
        }),
    }
}

pub async fn browser_evaluate(args: Value) -> anyhow::Result<ToolResult> {
    let script = args
        .get("script")
        .and_then(|v| v.as_str())
        .context("Missing 'script' parameter (JavaScript code)")?;

    let client = get_or_create_client().await?;
    let mut guard = client.lock().await;

    match guard.as_mut() {
        Some(c) => {
            // Execute JavaScript and get result
            let result: serde_json::Value = c
                .execute(script, vec![])
                .await
                .context("Failed to execute script")?;

            Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&result)?,
                error: None,
                metadata: Some(result),
            })
        }
        None => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Browser automation not available".to_string()),
            metadata: None,
        }),
    }
}

pub async fn browser_snapshot(args: Value) -> anyhow::Result<ToolResult> {
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as usize;

    let client = get_or_create_client().await?;
    let mut guard = client.lock().await;

    match guard.as_mut() {
        Some(c) => {
            // Get page source as a simple snapshot
            let html = c.source().await.context("Failed to get page source")?;

            // Extract text content (simplified)
            let text = html_to_text(&html);
            let truncated = text.lines().take(depth * 10).collect::<Vec<_>>().join("\n");

            Ok(ToolResult {
                success: true,
                output: truncated,
                error: None,
                metadata: Some(serde_json::json!({
                    "title": c.title().await.unwrap_or_default(),
                    "url": c.current_url().await?.to_string()
                })),
            })
        }
        None => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Browser automation not available".to_string()),
            metadata: None,
        }),
    }
}

pub async fn browser_close(_args: Value) -> anyhow::Result<ToolResult> {
    let client = get_or_create_client().await?;
    let mut guard = client.lock().await;

    if let Some(c) = guard.take() {
        c.close().await.ok();
    }

    Ok(ToolResult {
        success: true,
        output: "Browser closed".to_string(),
        error: None,
        metadata: None,
    })
}

/// Simple HTML to text conversion for accessibility snapshot
fn html_to_text(html: &str) -> String {
    let mut result = html.to_string();

    // Remove script and style tags
    result = regex::Regex::new(r"<script[^>]*>[\s\S]*?</script>")
        .unwrap()
        .replace_all(&result, "")
        .to_string();
    result = regex::Regex::new(r"<style[^>]*>[\s\S]*?</style>")
        .unwrap()
        .replace_all(&result, "")
        .to_string();

    // Convert some elements to text markers
    result = result.replace("<h1>", "\n# ").replace("</h1>", "\n");
    result = result.replace("<h2>", "\n## ").replace("</h2>", "\n");
    result = result.replace("<p>", "\n").replace("</p>", "\n");
    result = result.replace("<br>", "\n").replace("<br/>", "\n");
    result = result.replace("<li>", "\n- ").replace("</li>", "");

    // Remove all remaining tags
    result = regex::Regex::new(r"<[^>]+>")
        .unwrap()
        .replace_all(&result, "")
        .to_string();

    // Clean up whitespace
    result = regex::Regex::new(r"\n{3,}")
        .unwrap()
        .replace_all(&result, "\n\n")
        .to_string();

    result.trim().to_string()
}
