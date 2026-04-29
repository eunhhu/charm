use crate::providers::types::Message;
use std::collections::HashSet;

pub struct ContextCompressor;

impl ContextCompressor {
    pub fn compress(messages: &mut Vec<Message>, _total_tokens: usize, model_context_limit: usize) {
        let preserve_tokens = model_context_limit / 5;
        let max_messages = 32;

        if messages.len() <= 4 {
            return;
        }

        let system = if messages[0].role == "system" {
            Some(messages.remove(0))
        } else {
            None
        };

        let mut recent_token_count = 0usize;
        let mut preserve_count = 0usize;

        for msg in messages.iter().rev() {
            let msg_tokens = msg.content.as_ref().map(|c| c.len() / 4).unwrap_or(0);
            recent_token_count += msg_tokens;
            preserve_count += 1;
            if recent_token_count >= preserve_tokens || preserve_count >= max_messages {
                break;
            }
        }

        Self::compress_with_recent_count(messages, preserve_count.max(2));

        if let Some(sys) = system {
            messages.insert(0, sys);
        }
    }

    pub fn compact_now(messages: &mut Vec<Message>, preserve_recent: usize) -> usize {
        if messages.len() <= 4 {
            return 0;
        }

        let system = if messages[0].role == "system" {
            Some(messages.remove(0))
        } else {
            None
        };

        let removed = Self::compress_with_recent_count(messages, preserve_recent.max(2));

        if let Some(sys) = system {
            messages.insert(0, sys);
        }

        removed
    }

    pub fn compaction_raw(messages: &[Message], preserve_recent: usize) -> String {
        let working = if messages
            .first()
            .is_some_and(|message| message.role == "system")
        {
            &messages[1..]
        } else {
            messages
        };
        let to_compress = compaction_boundary(working, preserve_recent.max(2));
        if to_compress == 0 {
            return String::new();
        }

        working[..to_compress]
            .iter()
            .filter_map(raw_compaction_content)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn compress_with_recent_count(messages: &mut Vec<Message>, preserve_recent: usize) -> usize {
        let to_compress = compaction_boundary(messages, preserve_recent);
        if to_compress == 0 {
            return 0;
        }

        let compressed: Vec<Message> = messages.drain(0..to_compress).collect();
        let summary = Self::summarize(&compressed);

        messages.insert(
            0,
            Message {
                role: "assistant".to_string(),
                content: Some(format!("[Earlier: {}]", summary)),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            },
        );

        compressed.len()
    }

    fn summarize(msgs: &[Message]) -> String {
        let mut tool_calls = 0usize;
        let mut tool_successes = 0usize;
        let mut requests = Vec::new();
        let mut decisions = Vec::new();
        let mut files_read: HashSet<String> = HashSet::new();
        let mut files_edited: HashSet<String> = HashSet::new();
        let mut commands = Vec::new();

        for msg in msgs {
            match msg.role.as_str() {
                "user" => {
                    if let Some(content) = msg.content.as_deref() {
                        push_excerpt(&mut requests, content, 4);
                    }
                }
                "assistant" => {
                    if let Some(content) = msg.content.as_deref() {
                        push_excerpt(&mut decisions, content, 4);
                    }
                    if let Some(ref tc) = msg.tool_calls {
                        for call in tc {
                            tool_calls += 1;
                            let args =
                                serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                                    .ok();
                            match call.function.name.as_str() {
                                "read_range" | "grep_search" | "semantic_search" => {
                                    if let Some(args) = args.as_ref() {
                                        collect_tool_arg(args, "path", &mut files_read);
                                        collect_tool_arg(args, "file_path", &mut files_read);
                                        collect_tool_arg(args, "query", &mut files_read);
                                    }
                                }
                                "edit_patch" | "write_file" => {
                                    if let Some(args) = args.as_ref() {
                                        collect_tool_arg(args, "path", &mut files_edited);
                                        collect_tool_arg(args, "file_path", &mut files_edited);
                                        collect_tool_arg(args, "target_file", &mut files_edited);
                                    }
                                    if files_edited.is_empty() {
                                        files_edited.insert(call.function.name.clone());
                                    }
                                }
                                "run_command" => {
                                    if let Some(args) = args.as_ref() {
                                        collect_tool_arg(args, "command", &mut commands);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "tool" => {
                    if msg.content.as_ref().is_some_and(|content| {
                        content.contains("\"success\": true") || content.contains("success: true")
                    }) {
                        tool_successes += 1;
                    }
                }
                _ => {}
            }
        }

        let mut parts = Vec::new();
        if !requests.is_empty() {
            parts.push(format!("requests: {}", requests.join(" | ")));
        }
        if !decisions.is_empty() {
            parts.push(format!("assistant notes: {}", decisions.join(" | ")));
        }
        parts.push(format!("{tool_calls} tool calls, {tool_successes} ok"));
        if !files_read.is_empty() {
            parts.push(format!("refs: {}", sorted_join(files_read)));
        }
        if !files_edited.is_empty() {
            parts.push(format!("edited: {}", sorted_join(files_edited)));
        }
        if !commands.is_empty() {
            parts.push(format!("commands: {}", commands.join(" | ")));
        }

        parts.join("; ")
    }
}

fn compaction_boundary(messages: &[Message], preserve_recent: usize) -> usize {
    let mut to_compress = messages.len().saturating_sub(preserve_recent);
    while to_compress < messages.len() && messages[to_compress].role == "tool" {
        to_compress += 1;
    }
    to_compress
}

fn raw_compaction_content(message: &Message) -> Option<String> {
    let content = message.content.as_deref()?;
    if message.role != "tool" {
        return Some(content.to_string());
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
        let mut parts = Vec::new();
        if let Some(output) = value.get("output").and_then(serde_json::Value::as_str) {
            parts.push(output.to_string());
        }
        if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
            parts.push(error.to_string());
        }
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }

    Some(content.to_string())
}

fn push_excerpt(target: &mut Vec<String>, content: &str, max_items: usize) {
    if target.len() >= max_items {
        return;
    }
    let excerpt = content
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(content)
        .trim();
    if excerpt.is_empty() {
        return;
    }
    target.push(truncate_chars(excerpt, 140));
}

fn collect_tool_arg(args: &serde_json::Value, key: &str, target: &mut impl Extend<String>) {
    if let Some(value) = args.get(key).and_then(serde_json::Value::as_str) {
        target.extend([truncate_chars(value, 120)]);
    }
}

fn truncate_chars(raw: &str, max_chars: usize) -> String {
    if raw.chars().count() <= max_chars {
        return raw.to_string();
    }
    let mut truncated = raw.chars().take(max_chars).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn sorted_join(values: HashSet<String>) -> String {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort();
    values.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_message(role: &str, content: &str) -> Message {
        Message {
            role: role.to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        }
    }

    #[test]
    fn compact_now_rolls_old_messages_into_summary_and_preserves_recent() {
        let mut messages = vec![text_message("system", "system prompt")];
        for idx in 0..10 {
            messages.push(text_message("user", &format!("request {idx}")));
            messages.push(text_message("assistant", &format!("decision {idx}")));
        }

        let removed = ContextCompressor::compact_now(&mut messages, 6);

        assert_eq!(removed, 14);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "assistant");
        let summary = messages[1].content.as_deref().unwrap_or_default();
        assert!(summary.contains("[Earlier:"));
        assert!(summary.contains("request 0"));
        assert!(summary.contains("decision 0"));
        assert_eq!(messages.len(), 8);
        assert_eq!(
            messages.last().unwrap().content.as_deref(),
            Some("decision 9")
        );
    }
}
