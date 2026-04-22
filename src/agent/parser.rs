use crate::core::ToolCall;
use serde_json::Value;

pub struct ToolParser;

impl ToolParser {
    pub fn parse_tool_calls(message: &crate::providers::types::Message) -> Vec<ToolCall> {
        let mut calls = Vec::new();

        if let Some(tool_calls) = &message.tool_calls {
            for tc in tool_calls {
                if tc.r#type != "function" {
                    continue;
                }
                let name = &tc.function.name;
                let args: Value = match serde_json::from_str(&tc.function.arguments) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("Failed to parse tool args for {}: {}", name, e);
                        continue;
                    }
                };

                if let Some(call) = Self::map_to_tool_call(name, args) {
                    calls.push(call);
                }
            }
        }

        calls
    }

    fn map_to_tool_call(name: &str, args: Value) -> Option<ToolCall> {
        match name {
            "read_range" => Some(ToolCall::ReadRange {
                file_path: args["file_path"].as_str()?.to_string(),
                offset: args["offset"].as_u64().map(|v| v as usize),
                limit: args["limit"].as_u64().map(|v| v as usize),
            }),
            "read_symbol" => Some(ToolCall::ReadSymbol {
                file_path: args["file_path"].as_str()?.to_string(),
                symbol_name: args["symbol_name"].as_str()?.to_string(),
            }),
            "grep_search" => Some(ToolCall::GrepSearch {
                pattern: args["pattern"].as_str()?.to_string(),
                path: args["path"].as_str().map(|s| s.to_string()),
                include: args["include"].as_str().map(|s| s.to_string()),
                output_mode: parse_output_mode(args["output_mode"].as_str()),
            }),
            "glob_search" => Some(ToolCall::GlobSearch {
                pattern: args["pattern"].as_str()?.to_string(),
                path: args["path"].as_str().map(|s| s.to_string()),
            }),
            "list_dir" => Some(ToolCall::ListDir {
                dir_path: args["dir_path"].as_str()?.to_string(),
            }),
            "semantic_search" => Some(ToolCall::SemanticSearch {
                query: args["query"].as_str()?.to_string(),
                top_k: args["top_k"].as_u64().map(|v| v as usize),
                expand_full: args["expand_full"].as_bool(),
            }),
            "parallel_search" => Some(ToolCall::ParallelSearch {
                query: args["query"].as_str()?.to_string(),
                top_k: args["top_k"].as_u64().map(|v| v as usize),
            }),
            "edit_patch" => Some(ToolCall::EditPatch {
                file_path: args["file_path"].as_str()?.to_string(),
                old_string: args["old_string"].as_str()?.to_string(),
                new_string: args["new_string"].as_str()?.to_string(),
            }),
            "write_file" => Some(ToolCall::WriteFile {
                file_path: args["file_path"].as_str()?.to_string(),
                content: args["content"].as_str()?.to_string(),
            }),
            "run_command" => Some(ToolCall::RunCommand {
                command: args["command"].as_str()?.to_string(),
                cwd: args["cwd"].as_str().map(|s| s.to_string()),
                blocking: args["blocking"].as_bool().unwrap_or(true),
                timeout_ms: args["timeout_ms"].as_u64(),
                risk_class: parse_risk_class(args["risk_class"].as_str()),
            }),
            "checkpoint_create" => Some(ToolCall::CheckpointCreate {
                name: args["name"].as_str()?.to_string(),
                scope: parse_checkpoint_scope(args["scope"].as_str()),
            }),
            "checkpoint_restore" => Some(ToolCall::CheckpointRestore {
                checkpoint_id: args["checkpoint_id"].as_str()?.to_string(),
            }),
            "plan_update" => Some(ToolCall::PlanUpdate {
                objective: args["objective"].as_str().map(|s| s.to_string()),
                current_phase: args["current_phase"].as_str().map(|s| s.to_string()),
                completed_steps: args["completed_steps"].as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                }),
                blocked_steps: args["blocked_steps"].as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                }),
                notes: args["notes"].as_str().map(|s| s.to_string()),
            }),
            "memory_stage" => Some(ToolCall::MemoryStage {
                scope: parse_memory_scope(args["scope"].as_str()),
                category: args["category"].as_str()?.to_string(),
                content: args["content"].as_str()?.to_string(),
            }),
            "memory_commit" => Some(ToolCall::MemoryCommit {
                memory_ids: args["memory_ids"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
            }),
            _ => None,
        }
    }
}

fn parse_output_mode(mode: Option<&str>) -> crate::core::OutputMode {
    match mode {
        Some("files_with_matches") => crate::core::OutputMode::FilesWithMatches,
        Some("count") => crate::core::OutputMode::Count,
        _ => crate::core::OutputMode::Content,
    }
}

fn parse_risk_class(class: Option<&str>) -> crate::core::RiskClass {
    match class {
        Some("safe-read") => crate::core::RiskClass::SafeRead,
        Some("stateful-exec") => crate::core::RiskClass::StatefulExec,
        Some("destructive") => crate::core::RiskClass::Destructive,
        Some("external-side-effect") => crate::core::RiskClass::ExternalSideEffect,
        _ => crate::core::RiskClass::SafeExec,
    }
}

fn parse_checkpoint_scope(scope: Option<&str>) -> crate::core::CheckpointScope {
    match scope {
        Some("auto") => crate::core::CheckpointScope::Auto,
        Some("phase") => crate::core::CheckpointScope::Phase,
        _ => crate::core::CheckpointScope::Manual,
    }
}

fn parse_memory_scope(scope: Option<&str>) -> crate::core::MemoryScope {
    match scope {
        Some("project") => crate::core::MemoryScope::Project,
        Some("user") => crate::core::MemoryScope::User,
        _ => crate::core::MemoryScope::Session,
    }
}
