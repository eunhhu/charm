use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Map, Value};

use super::types::{
    ChatRequest, ChatResponse, Choice, CompletionTokensDetails, FunctionCall, Message,
    PromptTokensDetails, ToolCallBlock, Usage,
};

#[derive(Clone)]
pub struct OpenAiCodexClient {
    client: reqwest::Client,
    access_token: String,
    account_id: Option<String>,
    base_url: String,
}

impl OpenAiCodexClient {
    pub fn new(access_token: String, account_id: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            access_token,
            account_id,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
        }
    }

    pub async fn chat(&self, request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)> {
        let parsed = self.chat_raw(request).await?;
        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No choices in response"))?;

        Ok((choice.message, parsed.usage))
    }

    pub async fn chat_raw(&self, request: ChatRequest) -> anyhow::Result<ChatResponse> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.access_token))?,
        );
        if let Some(account_id) = &self.account_id {
            headers.insert("ChatGPT-Account-Id", HeaderValue::from_str(account_id)?);
        }

        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(&responses_payload(request))
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(anyhow::anyhow!("OpenAI Codex error {}: {}", status, body));
        }

        let value: Value = serde_json::from_str(&body)?;
        normalize_response(value)
    }
}

fn responses_payload(request: ChatRequest) -> Value {
    let mut payload = Map::new();
    payload.insert("model".to_string(), Value::String(request.model));

    let instructions = request
        .messages
        .iter()
        .filter(|message| message.role == "system")
        .filter_map(|message| message.content.clone())
        .filter(|content| !content.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !instructions.is_empty() {
        payload.insert("instructions".to_string(), Value::String(instructions));
    }

    payload.insert(
        "input".to_string(),
        Value::Array(response_input_items(&request.messages)),
    );

    if let Some(tools) = request.tools {
        payload.insert(
            "tools".to_string(),
            Value::Array(
                tools
                    .into_iter()
                    .map(|tool| {
                        serde_json::json!({
                            "type": "function",
                            "name": tool.function.name,
                            "description": tool.function.description,
                            "parameters": tool.function.parameters,
                        })
                    })
                    .collect(),
            ),
        );
    }

    payload.insert(
        "tool_choice".to_string(),
        Value::String(request.tool_choice.unwrap_or_else(|| "auto".to_string())),
    );
    payload.insert(
        "parallel_tool_calls".to_string(),
        Value::Bool(request.parallel_tool_calls.unwrap_or(true)),
    );
    payload.insert("store".to_string(), Value::Bool(false));
    payload.insert("stream".to_string(), Value::Bool(false));
    payload.insert("include".to_string(), Value::Array(Vec::new()));

    if let Some(temperature) = request.temperature
        && let Some(number) = serde_json::Number::from_f64(temperature as f64)
    {
        payload.insert("temperature".to_string(), Value::Number(number));
    }
    if let Some(max_tokens) = request.max_tokens {
        payload.insert(
            "max_output_tokens".to_string(),
            Value::Number(serde_json::Number::from(max_tokens)),
        );
    }
    if let Some(reasoning) = request.reasoning {
        payload.insert(
            "reasoning".to_string(),
            serde_json::json!({ "effort": reasoning.effort }),
        );
    }

    Value::Object(payload)
}

fn response_input_items(messages: &[Message]) -> Vec<Value> {
    let mut items = Vec::new();
    for message in messages {
        match message.role.as_str() {
            "system" => {}
            "user" => {
                if let Some(content) = message.content.as_ref()
                    && !content.trim().is_empty()
                {
                    items.push(text_item("user", "input_text", content));
                }
            }
            "assistant" => {
                if let Some(content) = message.content.as_ref()
                    && !content.trim().is_empty()
                {
                    items.push(text_item("assistant", "output_text", content));
                }
                if let Some(tool_calls) = message.tool_calls.as_ref() {
                    for call in tool_calls {
                        items.push(serde_json::json!({
                            "type": "function_call",
                            "name": call.function.name,
                            "arguments": call.function.arguments,
                            "call_id": call.id,
                        }));
                    }
                }
            }
            "tool" => {
                if let Some(call_id) = message.tool_call_id.as_ref() {
                    items.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": message.content.clone().unwrap_or_default(),
                    }));
                }
            }
            _ => {
                if let Some(content) = message.content.as_ref()
                    && !content.trim().is_empty()
                {
                    items.push(text_item("user", "input_text", content));
                }
            }
        }
    }
    items
}

fn text_item(role: &str, item_type: &str, text: &str) -> Value {
    serde_json::json!({
        "type": "message",
        "role": role,
        "content": [
            {
                "type": item_type,
                "text": text,
            }
        ]
    })
}

fn normalize_response(value: Value) -> anyhow::Result<ChatResponse> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("resp")
        .to_string();

    let mut content_parts = Vec::new();
    let mut tool_calls = Vec::new();
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for item in output {
            match item.get("type").and_then(Value::as_str) {
                Some("message")
                    if item.get("role").and_then(Value::as_str) == Some("assistant") =>
                {
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for block in content {
                            if block.get("type").and_then(Value::as_str) == Some("output_text")
                                && let Some(text) = block.get("text").and_then(Value::as_str)
                            {
                                content_parts.push(text.to_string());
                            }
                        }
                    }
                }
                Some("function_call") => {
                    let arguments = match item.get("arguments") {
                        Some(Value::String(value)) => value.clone(),
                        Some(other) => other.to_string(),
                        None => "{}".to_string(),
                    };
                    let call_id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or("call")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("function")
                        .to_string();

                    tool_calls.push(ToolCallBlock {
                        id: call_id,
                        r#type: "function".to_string(),
                        function: FunctionCall { name, arguments },
                    });
                }
                _ => {}
            }
        }
    }

    let message = Message {
        role: "assistant".to_string(),
        content: if content_parts.is_empty() {
            None
        } else {
            Some(content_parts.join("\n"))
        },
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
        reasoning: None,
        reasoning_details: None,
    };

    Ok(ChatResponse {
        id,
        choices: vec![Choice {
            message,
            finish_reason: None,
        }],
        usage: value.get("usage").and_then(parse_usage),
    })
}

fn parse_usage(usage: &Value) -> Option<Usage> {
    let prompt_tokens = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let completion_tokens = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or((prompt_tokens + completion_tokens) as u64) as u32;

    let cached_tokens = usage
        .get("input_tokens_details")
        .or_else(|| usage.get("prompt_tokens_details"))
        .and_then(Value::as_object)
        .and_then(|details| {
            details
                .get("cached_tokens")
                .or_else(|| details.get("cache_read_input_tokens"))
        })
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let reasoning_tokens = usage
        .get("output_tokens_details")
        .or_else(|| usage.get("completion_tokens_details"))
        .and_then(Value::as_object)
        .and_then(|details| {
            details
                .get("reasoning_tokens")
                .or_else(|| details.get("reasoning_output_tokens"))
        })
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;

    Some(Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cost: usage.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
        prompt_tokens_details: (cached_tokens > 0).then_some(PromptTokensDetails { cached_tokens }),
        completion_tokens_details: (reasoning_tokens > 0)
            .then_some(CompletionTokensDetails { reasoning_tokens }),
    })
}
