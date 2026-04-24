use crate::core::ToolResult;
use crate::tools::url_guard;
use anyhow::Context;
use regex::Regex;
use reqwest;
use serde_json::Value;
use std::time::Duration;

pub async fn fetch_url(args: Value) -> anyhow::Result<ToolResult> {
    let url = args
        .get("url")
        .and_then(|v| v.as_str())
        .context("Missing 'url' parameter")?;

    let validated_url = url_guard::validate_url(url)?;

    let max_length = args
        .get("max_length")
        .and_then(|v| v.as_u64())
        .unwrap_or(5000) as usize;

    let start_index = args
        .get("start_index")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let raw = args.get("raw").and_then(|v| v.as_bool()).unwrap_or(false);

    let timeout_secs = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);

    let client = validated_url
        .pin_dns(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(timeout_secs))
                .user_agent("Charm-Agent/0.1.0")
                .redirect(reqwest::redirect::Policy::none()),
        )
        .build()
        .context("Failed to build HTTP client")?;

    let response = client
        .get(url)
        .send()
        .await
        .context("Failed to send HTTP request")?;

    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(k, v)| format!("{}: {}", k, v.to_str().unwrap_or("[binary]")))
        .collect::<Vec<_>>()
        .join("\n");

    let body = if raw {
        response
            .text()
            .await
            .context("Failed to read response body")?
    } else {
        let html = response
            .text()
            .await
            .context("Failed to read response body")?;
        html_to_markdown(&html)
    };

    // Apply start_index and max_length (UTF-8-safe)
    let content = if start_index >= body.len() {
        String::new()
    } else {
        url_guard::safe_slice(&body, start_index, start_index + max_length).to_string()
    };

    let truncated = body.len() > (start_index + max_length);

    let output = format!(
        "Status: {}\n\nHeaders:\n{}\n\n{}Content:\n{}{}",
        status,
        headers,
        if truncated {
            format!(
                "(showing bytes {}-{} of {})\n",
                start_index,
                start_index + content.len(),
                body.len()
            )
        } else {
            String::new()
        },
        content,
        if truncated {
            "\n\n[Content truncated...]"
        } else {
            ""
        }
    );

    Ok(ToolResult {
        success: status.is_success(),
        output,
        error: if status.is_success() {
            None
        } else {
            Some(format!("HTTP error: {}", status))
        },
        metadata: Some(serde_json::json!({
            "url": url,
            "status_code": status.as_u16(),
            "content_length": body.len(),
            "truncated": truncated
        })),
    })
}

pub async fn http_request(args: Value) -> anyhow::Result<ToolResult> {
    let url = args
        .get("url")
        .and_then(|v| v.as_str())
        .context("Missing 'url' parameter")?;

    let validated_url = url_guard::validate_url(url)?;

    let method = args
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_uppercase();

    let headers = args.get("headers").cloned().unwrap_or(Value::Null);
    let body = args.get("body").and_then(|v| v.as_str());

    let timeout_secs = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);

    let client = validated_url
        .pin_dns(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(timeout_secs))
                .user_agent("Charm-Agent/0.1.0")
                .redirect(reqwest::redirect::Policy::none()),
        )
        .build()
        .context("Failed to build HTTP client")?;

    let mut request_builder = match method.as_str() {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "PATCH" => client.patch(url),
        "DELETE" => client.delete(url),
        "HEAD" => client.head(url),
        _ => client.request(reqwest::Method::from_bytes(method.as_bytes())?, url),
    };

    // Add headers
    if let Some(obj) = headers.as_object() {
        for (key, value) in obj {
            if let Some(val_str) = value.as_str() {
                request_builder = request_builder.header(key, val_str);
            }
        }
    }

    // Add body for POST/PUT/PATCH
    if let Some(body_str) = body {
        if method == "POST" || method == "PUT" || method == "PATCH" {
            request_builder = request_builder.body(body_str.to_string());
        }
    }

    let response = request_builder
        .send()
        .await
        .context("Failed to send HTTP request")?;

    let status = response.status();
    let response_headers: Vec<String> = response
        .headers()
        .iter()
        .map(|(k, v)| format!("{}: {}", k, v.to_str().unwrap_or("[binary]")))
        .collect();

    let response_body = response.text().await.unwrap_or_default();

    let output = format!(
        "HTTP {}\n\nHeaders:\n{}\n\nBody:\n{}",
        status,
        response_headers.join("\n"),
        url_guard::truncate_str(&response_body, 10000)
    );

    Ok(ToolResult {
        success: status.is_success(),
        output,
        error: if status.is_success() {
            None
        } else {
            Some(format!("HTTP error: {}", status))
        },
        metadata: Some(serde_json::json!({
            "url": url,
            "method": method,
            "status_code": status.as_u16()
        })),
    })
}

/// Convert HTML to markdown for better readability
fn html_to_markdown(html: &str) -> String {
    // Basic HTML to markdown conversion
    let mut result = html.to_string();

    // Remove script and style tags with content
    result = Regex::new(r"<script[^>]*>[\s\S]*?</script>")
        .unwrap()
        .replace_all(&result, "")
        .to_string();
    result = Regex::new(r"<style[^>]*>[\s\S]*?</style>")
        .unwrap()
        .replace_all(&result, "")
        .to_string();

    // Convert common HTML elements
    let conversions = [
        ("<h1[^>]*>(.*?)</h1>", "# $1\n\n"),
        ("<h2[^>]*>(.*?)</h2>", "## $1\n\n"),
        ("<h3[^>]*>(.*?)</h3>", "### $1\n\n"),
        ("<h4[^>]*>(.*?)</h4>", "#### $1\n\n"),
        ("<h5[^>]*>(.*?)</h5>", "##### $1\n\n"),
        ("<h6[^>]*>(.*?)</h6>", "###### $1\n\n"),
        ("<p[^>]*>(.*?)</p>", "$1\n\n"),
        ("<br\\s*/?>", "\n"),
        ("<strong[^>]*>(.*?)</strong>", "**$1**"),
        ("<b[^>]*>(.*?)</b>", "**$1**"),
        ("<em[^>]*>(.*?)</em>", "*$1*"),
        ("<i[^>]*>(.*?)</i>", "*$1*"),
        ("<code[^>]*>(.*?)</code>", "`$1`"),
        ("<pre[^>]*>(.*?)</pre>", "```\n$1\n```\n\n"),
        ("<a[^>]+href=['\"]([^'\"]+)['\"][^>]*>(.*?)</a>", "[$2]($1)"),
        ("<li[^>]*>(.*?)</li>", "- $1\n"),
        ("<ul[^>]*>(.*?)</ul>", "$1\n"),
        ("<ol[^>]*>(.*?)</ol>", "$1\n"),
    ];

    for (pattern, replacement) in &conversions {
        result = Regex::new(pattern)
            .unwrap()
            .replace_all(&result, *replacement)
            .to_string();
    }

    // Remove remaining HTML tags
    result = Regex::new(r"<[^>]+>")
        .unwrap()
        .replace_all(&result, "")
        .to_string();

    // Clean up whitespace
    result = Regex::new(r"\n{3,}")
        .unwrap()
        .replace_all(&result, "\n\n")
        .to_string();

    result.trim().to_string()
}
