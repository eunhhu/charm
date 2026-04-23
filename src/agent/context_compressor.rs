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

        let to_compress = messages.len().saturating_sub(preserve_count.max(2));
        if to_compress == 0 {
            if let Some(sys) = system {
                messages.insert(0, sys);
            }
            return;
        }

        let ids_to_remove: HashSet<String> = messages[..to_compress]
            .iter()
            .filter(|m| m.role == "assistant")
            .filter_map(|m| m.tool_calls.as_ref())
            .flatten()
            .map(|tc| tc.id.clone())
            .collect();

        let compressed: Vec<Message> = messages
            .drain(0..to_compress)
            .filter(|msg| {
                if msg.role == "tool" {
                    if let Some(ref id) = msg.tool_call_id {
                        return !ids_to_remove.contains(id);
                    }
                    return false;
                }
                true
            })
            .collect();

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

        if let Some(sys) = system {
            messages.insert(0, sys);
        }
    }

    fn summarize(msgs: &[Message]) -> String {
        let mut tool_calls = 0usize;
        let mut tool_successes = 0usize;
        let _files_read: HashSet<String> = HashSet::new();
        let mut files_edited: HashSet<String> = HashSet::new();

        for msg in msgs {
            match msg.role.as_str() {
                "assistant" => {
                    if let Some(ref tc) = msg.tool_calls {
                        for call in tc {
                            match call.function.name.as_str() {
                                "read_range" | "grep_search" | "semantic_search" => {
                                    tool_calls += 1;
                                }
                                "edit_patch" | "write_file" => {
                                    tool_calls += 1;
                                    files_edited.insert(call.function.name.clone());
                                }
                                _ => {
                                    tool_calls += 1;
                                }
                            }
                        }
                    }
                }
                "tool" => {
                    if let Some(ref content) = msg.content {
                        if content.contains("\"success\": true")
                            || content.contains("success: true")
                        {
                            tool_successes += 1;
                        }
                    }
                }
                _ => {}
            }
        }

        if files_edited.is_empty() {
            format!("{} calls, {} ok", tool_calls, tool_successes)
        } else {
            format!(
                "{} calls, {} ok, edited: {}",
                tool_calls,
                tool_successes,
                files_edited.len()
            )
        }
    }
}
