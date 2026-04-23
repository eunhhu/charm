use crate::providers::types::{
    ChatResponse, Choice, Message, Usage,
};
use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct StreamChunk {
    pub id: Option<String>,
    pub object: Option<String>,
    pub created: Option<u64>,
    pub model: Option<String>,
    pub choices: Vec<StreamChoice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamChoice {
    pub index: u32,
    pub delta: StreamDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StreamDelta {
    pub role: Option<String>,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamToolCall {
    pub index: u32,
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub call_type: Option<String>,
    pub function: Option<StreamFunction>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamFunction {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

pub fn parse_sse_line(line: &str) -> Option<Result<StreamChunk>> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with(':') {
        return None;
    }
    if let Some(data) = trimmed.strip_prefix("data:") {
        let data = data.trim();
        if data == "[DONE]" {
            return None;
        }
        match serde_json::from_str::<StreamChunk>(data) {
            Ok(chunk) => Some(Ok(chunk)),
            Err(e) => Some(Err(anyhow::anyhow!(
                "SSE JSON parse error: {} - data: {}",
                e,
                data
            ))),
        }
    } else {
        None
    }
}

pub fn parse_sse_stream(body: &str) -> Vec<StreamChunk> {
    body.lines()
        .filter_map(|line| parse_sse_line(line))
        .filter_map(|result| result.ok())
        .collect()
}

pub fn accumulate_stream_to_response(chunks: &[StreamChunk]) -> Result<ChatResponse> {
    let mut content = String::new();
    let mut role = "assistant".to_string();
    let mut tool_calls_map: std::collections::HashMap<u32, (String, String, String)> =
        std::collections::HashMap::new();
    let mut id = String::new();
    let mut finish_reason = None::<String>;
    let mut usage = None::<Usage>;

    for chunk in chunks {
        if let Some(ref chunk_id) = chunk.id {
            id = chunk_id.clone();
        }
        if let Some(ref _chunk_model) = chunk.model {
            // Model tracking disabled - add when needed
        }
        if let Some(ref chunk_usage) = chunk.usage {
            usage = Some(chunk_usage.clone());
        }
        for choice in &chunk.choices {
            let delta = &choice.delta;
            if let Some(ref delta_role) = delta.role {
                role = delta_role.clone();
            }
            if let Some(ref delta_content) = delta.content {
                content.push_str(delta_content);
            }
            if let Some(ref finish) = choice.finish_reason {
                finish_reason = Some(finish.clone());
            }
            if let Some(ref calls) = delta.tool_calls {
                for call in calls {
                    let entry = tool_calls_map
                        .entry(call.index)
                        .or_insert_with(|| (String::new(), String::new(), String::new()));
                    if let Some(ref call_id) = call.id {
                        entry.0 = call_id.clone();
                    }
                    if let Some(ref func) = call.function {
                        if let Some(ref name) = func.name {
                            entry.1 = name.clone();
                        }
                        if let Some(ref args) = func.arguments {
                            entry.2.push_str(args);
                        }
                    }
                }
            }
        }
    }

    let message = Message {
        role: role.clone(),
        content: if content.is_empty() {
            None
        } else {
            Some(content)
        },
        tool_calls: if tool_calls_map.is_empty() {
            None
        } else {
            use crate::providers::types::{FunctionCall, ToolCallBlock};
            let mut calls: Vec<ToolCallBlock> = tool_calls_map
                .into_iter()
                .map(|(_, (id, name, arguments))| ToolCallBlock {
                    id,
                    r#type: "function".to_string(),
                    function: FunctionCall { name, arguments },
                })
                .collect();
            calls.sort_by_key(|c| c.id.clone());
            Some(calls)
        },
        tool_call_id: None,
        reasoning: None,
        reasoning_details: None,
    };

    Ok(ChatResponse {
        id,
        choices: vec![Choice {
            message,
            finish_reason: Some(finish_reason.unwrap_or_else(|| "stop".to_string())),
        }],
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_content_chunk() {
        let line = r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1234,"model":"gpt-4o","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
    }

    #[test]
    fn parse_done_line() {
        let line = "data: [DONE]";
        assert!(parse_sse_line(line).is_none());
    }

    #[test]
    fn parse_role_chunk() {
        let line = r#"data: {"id":"chatcmpl-1","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        assert_eq!(chunk.choices[0].delta.role.as_deref(), Some("assistant"));
    }

    #[test]
    fn accumulate_simple_response() {
        let chunks = vec![
            StreamChunk {
                id: Some("chatcmpl-1".to_string()),
                object: Some("chat.completion.chunk".to_string()),
                created: Some(1234),
                model: Some("gpt-4o".to_string()),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: Some("assistant".to_string()),
                        content: Some("Hello".to_string()),
                        tool_calls: None,
                    },
                    finish_reason: None,
                }],
                usage: None,
            },
            StreamChunk {
                id: Some("chatcmpl-1".to_string()),
                object: Some("chat.completion.chunk".to_string()),
                created: Some(1234),
                model: Some("gpt-4o".to_string()),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: None,
                        content: Some(" world".to_string()),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: None,
            },
        ];
        let response = accumulate_stream_to_response(&chunks).unwrap();
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("Hello world")
        );
        assert_eq!(response.choices[0].message.role, "assistant");
    }
}
