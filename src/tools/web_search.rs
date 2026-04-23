use crate::core::ToolResult;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub link: String,
    pub snippet: String,
    pub source: String,
}

pub async fn search_web(args: Value) -> anyhow::Result<ToolResult> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .context("Missing 'query' parameter")?;

    let domain = args.get("domain").and_then(|v| v.as_str());

    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    let num_results = args
        .get("num_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;

    // Try DuckDuckGo first (no API key required)
    let results = search_duckduckgo(query, domain, num_results).await?;

    if results.is_empty() {
        return Ok(ToolResult {
            success: true,
            output: format!("No results found for: {}", query),
            error: None,
            metadata: Some(serde_json::json!({
                "query": query,
                "results": 0
            })),
        });
    }

    let limited = &results[..limit.min(results.len())];

    let mut output = format!("Found {} results for '{}':\n\n", limited.len(), query);

    for (idx, result) in limited.iter().enumerate() {
        output.push_str(&format!(
            "[{}] {}\n  URL: {}\n  Snippet: {}\n\n",
            idx + 1,
            result.title,
            result.link,
            result.snippet
        ));
    }

    Ok(ToolResult {
        success: true,
        output,
        error: None,
        metadata: Some(serde_json::json!({
            "query": query,
            "results": limited.len()
        })),
    })
}

async fn search_duckduckgo(
    query: &str,
    domain: Option<&str>,
    _limit: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    let search_query = if let Some(d) = domain {
        format!("{} site:{}", query, d)
    } else {
        query.to_string()
    };

    // DuckDuckGo HTML search (no API key required)
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(&search_query)
    );

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let response = client.get(&url).send().await?;

    if !response.status().is_success() {
        return Ok(vec![]);
    }

    let html = response.text().await?;
    let results = parse_duckduckgo_results(&html);

    Ok(results)
}

fn parse_duckduckgo_results(html: &str) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // Simple regex-based parsing for DuckDuckGo HTML results
    let title_re = regex::Regex::new(r#"<a[^>]+class="result__a"[^>]*>([^<]+)</a>"#).unwrap();
    let link_re = regex::Regex::new(r#"<a[^>]+class="result__a"[^>]+href="([^"]+)""#).unwrap();
    let snippet_re =
        regex::Regex::new(r#"<a[^>]+class="result__snippet"[^>]*>([^<]+)</a>"#).unwrap();

    let titles: Vec<String> = title_re
        .captures_iter(html)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .collect();

    let links: Vec<String> = link_re
        .captures_iter(html)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .collect();

    let snippets: Vec<String> = snippet_re
        .captures_iter(html)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .collect();

    let min_len = titles.len().min(links.len()).min(snippets.len());

    for i in 0..min_len {
        let link = links[i]
            .replace("//duckduckgo.com/l/?uddg=", "")
            .replace("&amp;rut=", "?rut=");

        // URL decode
        let decoded_link = match urlencoding::decode(&link) {
            Ok(d) => d.to_string(),
            Err(_) => link,
        };

        results.push(SearchResult {
            title: html_escape::decode_html_entities(&titles[i]).to_string(),
            link: decoded_link,
            snippet: html_escape::decode_html_entities(&snippets[i]).to_string(),
            source: "DuckDuckGo".to_string(),
        });
    }

    results
}

pub async fn fetch_search_result(args: Value) -> anyhow::Result<ToolResult> {
    let url = args
        .get("url")
        .and_then(|v| v.as_str())
        .context("Missing 'url' parameter")?;

    let extract_content = args
        .get("extract_content")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let response = client
        .get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        )
        .send()
        .await
        .context("Failed to fetch URL")?;

    if !response.status().is_success() {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("HTTP error: {}", response.status())),
            metadata: None,
        });
    }

    let html = response
        .text()
        .await
        .context("Failed to read response body")?;
    let html_len = html.len();

    let content = if extract_content {
        extract_main_content(&html)
    } else {
        html
    };

    let truncated = if content.len() > 10000 {
        format!(
            "{}...\n\n[Content truncated to 10000 characters]",
            &content[..10000]
        )
    } else {
        content
    };

    Ok(ToolResult {
        success: true,
        output: truncated,
        error: None,
        metadata: Some(serde_json::json!({
            "url": url,
            "length": html_len
        })),
    })
}

fn extract_main_content(html: &str) -> String {
    let mut content = html.to_string();

    // Remove script, style, nav, header, footer
    for tag in ["script", "style", "nav", "header", "footer"] {
        let re = regex::Regex::new(&format!(r#"<{}[^>]*>[\s\S]*?</{}>"#, tag, tag)).unwrap();
        content = re.replace_all(&content, "").to_string();
    }

    // Try to extract main/article content
    let main_re = regex::Regex::new(r#"<main[^>]*>([\s\S]*?)</main>"#).unwrap();
    let article_re = regex::Regex::new(r#"<article[^>]*>([\s\S]*?)</article>"#).unwrap();

    let content = if let Some(caps) = main_re.captures(&content) {
        caps.get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or(content)
    } else if let Some(caps) = article_re.captures(&content) {
        caps.get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or(content)
    } else {
        content
    };

    // Convert to text
    let content = regex::Regex::new(r#"<[^>]+>"#)
        .unwrap()
        .replace_all(&content, " ")
        .to_string();

    // Clean up whitespace
    let content = regex::Regex::new(r"\s+")
        .unwrap()
        .replace_all(&content, " ")
        .to_string();

    content.trim().to_string()
}
