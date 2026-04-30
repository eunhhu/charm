use crate::agent::parser::ToolParser;
use crate::agent::prompt::{AgentMode, PromptAssembler};
use crate::core::{ToolCall, ToolResult, WorkspaceState};
use crate::harness::{
    SessionStore,
    session::{Session, SessionStatus},
};
use crate::prism::DependencyGraph;
use crate::providers::client::ProviderClient;
use crate::providers::types::{ChatRequest, Message, ReasoningConfig};
use crate::tools::ToolRegistry;
use crate::tui;
use std::collections::HashSet;
use std::path::Path;
use uuid::Uuid;

pub struct AgentRunner {
    client: ProviderClient,
    registry: ToolRegistry,
    assembler: PromptAssembler,
    workspace: WorkspaceState,
    tool_budget: usize,
    model: String,
    workspace_root: std::path::PathBuf,
    prism_graph: Option<DependencyGraph>,
    touched_files: HashSet<String>,
}

impl AgentRunner {
    pub fn new(
        client: ProviderClient,
        workspace_root: &Path,
        request_model: String,
        prompt_model_hint: String,
        mode: AgentMode,
    ) -> anyhow::Result<Self> {
        let registry = ToolRegistry::new(workspace_root);
        let workspace = crate::core::detect_workspace(workspace_root)?;

        let prism_graph = DependencyGraph::analyze_workspace(workspace_root).ok();
        if let Some(ref graph) = prism_graph {
            println!(
                "[Prism] Analyzed workspace: {} files, {} edges",
                graph.node_count(),
                graph.edge_count()
            );
        }

        Ok(Self {
            client,
            registry,
            assembler: PromptAssembler::new(workspace_root)
                .with_provider(&prompt_model_hint)
                .with_mode(mode),
            workspace,
            tool_budget: 20,
            model: request_model,
            workspace_root: workspace_root.to_path_buf(),
            prism_graph,
            touched_files: HashSet::new(),
        })
    }

    pub async fn run_task(&mut self, task: &str) -> anyhow::Result<Vec<ToolResult>> {
        let mut messages: Vec<Message> = Vec::new();
        let system_prompt = self.assembler.assemble(&self.workspace, task, Vec::new());

        messages.push(Message {
            role: "system".to_string(),
            content: Some(system_prompt),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        });

        messages.push(Message {
            role: "user".to_string(),
            content: Some(task.to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        });

        let mut all_results: Vec<ToolResult> = Vec::new();
        let mut turn_count = 0;
        let max_turns = 10;
        let store = SessionStore::new(&self.workspace_root);

        while turn_count < max_turns {
            turn_count += 1;
            println!(
                "{}",
                tui::turn_header(turn_count, max_turns, self.tool_budget)
            );

            let session = Session {
                session_id: Uuid::new_v4().to_string(),
                task: task.to_string(),
                messages: messages.clone(),
                tool_budget_used: 20 - self.tool_budget,
                turn_count,
                status: SessionStatus::Active,
            };
            let _ = store.save(&session);

            self.inject_prism_context(&mut messages);

            let request = ChatRequest {
                model: self.model.clone(),
                messages: messages.clone(),
                tools: Some(self.client.build_tool_schemas()),
                tool_choice: Some("auto".to_string()),
                temperature: Some(0.2),
                max_tokens: Some(8000),
                reasoning: if self.model.contains("gpt-5") || self.model.contains("claude-opus") {
                    Some(ReasoningConfig {
                        effort: "high".to_string(),
                    })
                } else {
                    None
                },
                parallel_tool_calls: Some(true),
                stream: Some(false),
            };

            let spinner = tui::Spinner::new("Thinking...");
            let (response, usage) = self.client.chat(request).await?;
            spinner.finish("Done");

            if let Some(u) = usage {
                println!(
                    "  {}",
                    tui::token_display(
                        u.prompt_tokens,
                        u.completion_tokens,
                        u.completion_tokens_details
                            .as_ref()
                            .map(|d| d.reasoning_tokens)
                            .unwrap_or(0),
                    )
                );
            }

            if let Some(content) = &response.content {
                println!(
                    "{}",
                    tui::agent_thought(content.lines().next().unwrap_or("(thinking...)"))
                );
            }

            let parsed_calls = ToolParser::parse_tool_calls_with_ids(&response);
            if parsed_calls.is_empty() {
                println!("  No tool calls. Task complete or agent stopped.");
                break;
            }

            let all_tool_call_ids: Vec<String> = response
                .tool_calls
                .as_ref()
                .map(|tcs| tcs.iter().map(|tc| tc.id.clone()).collect())
                .unwrap_or_default();

            let mut tool_results: Vec<(String, ToolResult)> = Vec::new();
            let mut budget_exhausted = false;

            for parsed in parsed_calls {
                let id = parsed.id;
                let call = parsed.call;

                if self.tool_budget == 0 {
                    println!("  Tool budget exhausted.");
                    budget_exhausted = true;
                    tool_results.push((
                        id,
                        ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("Tool budget exhausted".to_string()),
                            metadata: None,
                        },
                    ));
                    continue;
                }
                self.tool_budget -= 1;

                let tool_name = match &call {
                    ToolCall::ReadRange { .. } => "read_range",
                    ToolCall::GrepSearch { .. } => "grep_search",
                    ToolCall::GlobSearch { .. } => "glob_search",
                    ToolCall::ListDir { .. } => "list_dir",
                    ToolCall::SemanticSearch { .. } => "semantic_search",
                    ToolCall::ParallelSearch { .. } => "parallel_search",
                    ToolCall::EditPatch { .. } => "edit_patch",
                    ToolCall::WriteFile { .. } => "write_file",
                    ToolCall::RunCommand { .. } => "run_command",
                    ToolCall::PollCommand { .. } => "poll_command",
                    ToolCall::CancelCommand { .. } => "cancel_command",
                    ToolCall::CheckpointCreate { .. } => "checkpoint_create",
                    ToolCall::CheckpointRestore { .. } => "checkpoint_restore",
                    ToolCall::PlanUpdate { .. } => "plan_update",
                    ToolCall::MemoryStage { .. } => "memory_stage",
                    ToolCall::MemoryCommit { .. } => "memory_commit",
                    _ => {
                        tool_results.push((
                            id,
                            ToolResult {
                                success: false,
                                output: String::new(),
                                error: Some("Unrecognized tool variant".to_string()),
                                metadata: None,
                            },
                        ));
                        continue;
                    }
                };

                match &call {
                    ToolCall::ReadRange { file_path, .. }
                    | ToolCall::EditPatch { file_path, .. }
                    | ToolCall::WriteFile { file_path, .. } => {
                        self.touched_files.insert(file_path.clone());
                    }
                    _ => {}
                }

                println!(
                    "{}",
                    tui::tool_call(tool_name, &serde_json::to_string(&call).unwrap_or_default())
                );
                let args = serde_json::to_value(&call)?;
                match self.registry.execute(tool_name, args).await {
                    Ok(result) => {
                        println!(
                            "{}",
                            tui::tool_success(result.output.lines().next().unwrap_or("ok"))
                        );
                        if let Some(ref meta) = result.metadata {
                            if let Some(path) = meta.get("file_path").and_then(|v| v.as_str()) {
                                self.touched_files.insert(path.to_string());
                            }
                            if let Some(path) = meta.get("resolved_path").and_then(|v| v.as_str())
                                && let Ok(rel) =
                                    std::path::Path::new(path).strip_prefix(&self.workspace_root)
                            {
                                self.touched_files.insert(rel.to_string_lossy().to_string());
                            }
                        }
                        tool_results.push((id, result.clone()));
                        all_results.push(result);
                    }
                    Err(e) => {
                        println!("{}", tui::tool_error(&e.to_string()));
                        tool_results.push((
                            id,
                            ToolResult {
                                success: false,
                                output: String::new(),
                                error: Some(e.to_string()),
                                metadata: None,
                            },
                        ));
                    }
                }
            }

            let result_ids: HashSet<String> =
                tool_results.iter().map(|(id, _)| id.clone()).collect();
            for id in &all_tool_call_ids {
                if !result_ids.contains(id) {
                    tool_results.push((
                        id.clone(),
                        ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("Tool call parsing failed".to_string()),
                            metadata: None,
                        },
                    ));
                }
            }

            messages.push(Message {
                role: "assistant".to_string(),
                content: response.content,
                tool_calls: response.tool_calls,
                tool_call_id: None,
                reasoning: response.reasoning,
                reasoning_details: response.reasoning_details,
            });

            for (id, result) in tool_results {
                messages.push(Message {
                    role: "tool".to_string(),
                    content: Some(serde_json::to_string(&result)?),
                    tool_calls: None,
                    tool_call_id: Some(id),
                    reasoning: None,
                    reasoning_details: None,
                });
            }

            if budget_exhausted {
                break;
            }
        }

        let final_session = Session {
            session_id: Uuid::new_v4().to_string(),
            task: task.to_string(),
            messages: messages.clone(),
            tool_budget_used: 20 - self.tool_budget,
            turn_count,
            status: SessionStatus::Completed,
        };
        let _ = store.save(&final_session);

        Ok(all_results)
    }

    fn inject_prism_context(&self, messages: &mut Vec<Message>) {
        let graph = match &self.prism_graph {
            Some(g) => g,
            None => return,
        };

        let mut related = HashSet::new();
        for file in &self.touched_files {
            for r in graph.get_related_files(file, 1) {
                related.insert(r);
            }
        }

        related.retain(|f| !self.touched_files.contains(f));
        if related.is_empty() {
            return;
        }

        let files: Vec<String> = related.into_iter().take(8).collect();
        let content = format!(
            "[Prism] Files related to your recent work: {}",
            files.join(", ")
        );

        messages.push(Message {
            role: "user".to_string(),
            content: Some(content),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        });
    }
}
