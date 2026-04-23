use super::mcp::{call_mcp_tool, discover_mcp_tools, refresh_mcp_snapshot};
use super::router::{decide_intent, parse_slash_override, requires_approval};
use super::types::{
    ApprovalRequest, ApprovalStatus, AutonomyLevel, LspSnapshot, McpSnapshot, RouterIntent,
    RuntimeEvent, SessionLifecycle, ToolExecution, WorkspacePreflight,
};
use super::workspace::{build_preflight, collect_lsp_snapshot, refresh_lsp_snapshot};
use crate::agent::parser::ToolParser;
use crate::agent::prompt::PromptAssembler;
use crate::cli::InteractiveRequest;
use crate::core::{RiskClass, ToolCall, ToolResult, WorkspaceState};
use crate::harness::PlanManager;
use crate::harness::session::{
    SessionMetadata, SessionSelection, SessionSnapshot, SessionStatus, SessionStore,
    TranscriptEntry,
};
use crate::providers::client::ProviderClient;
use crate::providers::sse::{StreamChunk, accumulate_stream_to_response};
use crate::providers::types::{ChatRequest, Message, ToolSchema, Usage};
use crate::tools::ToolRegistry;
use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use uuid::Uuid;

#[async_trait]
pub trait RuntimeModel: Send + Sync {
    async fn chat(&self, request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)>;
    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamChunk>>>;
    fn tool_schemas(&self) -> Vec<ToolSchema>;
}

#[async_trait]
impl RuntimeModel for ProviderClient {
    async fn chat(&self, request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)> {
        ProviderClient::chat(self, request).await
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamChunk>>> {
        ProviderClient::chat_stream(self, request).await
    }

    fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.build_tool_schemas()
    }
}

pub struct SessionRuntime {
    workspace_root: PathBuf,
    workspace_state: WorkspaceState,
    model_name: String,
    autonomy: AutonomyLevel,
    store: SessionStore,
    plan_manager: PlanManager,
    registry: ToolRegistry,
    prompt_assembler: PromptAssembler,
    model: Arc<dyn RuntimeModel>,
    snapshot: SessionSnapshot,
    preflight: WorkspacePreflight,
    lsp: LspSnapshot,
    mcp: McpSnapshot,
}

impl SessionRuntime {
    pub async fn bootstrap(
        workspace_root: &Path,
        model_name: String,
        provider_hint: String,
        request: InteractiveRequest,
        model: Arc<dyn RuntimeModel>,
    ) -> anyhow::Result<(Self, Vec<RuntimeEvent>)> {
        let store = SessionStore::new(workspace_root);
        let workspace_state = detect_workspace(workspace_root)?;
        let preflight = build_preflight(
            workspace_root,
            workspace_state.branch.clone(),
            workspace_state.dirty_files.clone(),
            None,
        );
        let lsp = collect_lsp_snapshot(workspace_root);
        let mcp = discover_mcp_tools(workspace_root);

        let selection = if request.new_session {
            SessionSelection::New
        } else if let Some(session_id) = request.session_id.clone() {
            match store.load_snapshot(&session_id)? {
                Some(snapshot) => SessionSelection::Existing(snapshot.metadata),
                None => SessionSelection::New,
            }
        } else if request.continue_last {
            store.smart_continue()?
        } else {
            store.smart_continue()?
        };

        let mut snapshot = match selection {
            SessionSelection::Existing(meta) => store
                .load_snapshot(&meta.session_id)?
                .with_context(|| format!("missing session snapshot for {}", meta.session_id))?,
            SessionSelection::New => new_session_snapshot(workspace_root, request.prompt.clone()),
        };

        if snapshot.messages.is_empty() || snapshot.messages[0].role != "system" {
            snapshot.messages.insert(
                0,
                Message {
                    role: "system".to_string(),
                    content: Some(String::new()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning: None,
                    reasoning_details: None,
                },
            );
        }

        let mut runtime = Self {
            workspace_root: workspace_root.to_path_buf(),
            workspace_state,
            model_name,
            autonomy: AutonomyLevel::Aggressive,
            store,
            plan_manager: PlanManager::new(workspace_root),
            registry: ToolRegistry::new(workspace_root),
            prompt_assembler: PromptAssembler::new(workspace_root).with_provider(&provider_hint),
            model,
            snapshot,
            preflight,
            lsp,
            mcp,
        };
        runtime.refresh_system_prompt();
        runtime.save()?;

        let lifecycle = if runtime.snapshot.transcript.is_empty() {
            SessionLifecycle::Started
        } else {
            SessionLifecycle::Resumed
        };
        let events = runtime.initial_events(lifecycle);
        Ok((runtime, events))
    }

    pub fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn lsp(&self) -> &LspSnapshot {
        &self.lsp
    }

    pub fn mcp(&self) -> &McpSnapshot {
        &self.mcp
    }

    pub fn preflight(&self) -> &WorkspacePreflight {
        &self.preflight
    }

    pub async fn submit_input(&mut self, input: &str) -> anyhow::Result<Vec<RuntimeEvent>> {
        if let Some(events) = self.handle_internal_command(input).await? {
            self.save()?;
            return Ok(events);
        }

        let (override_intent, body) = parse_slash_override(input);
        let message = body.trim();
        let has_plan = self.plan_manager.load().objective != "(none)";
        let decision = decide_intent(
            message,
            override_intent,
            has_plan,
            !self.lsp.diagnostics.is_empty(),
        );

        self.snapshot.metadata.router_intent = decision.intent;
        self.snapshot.metadata.last_active_at = Utc::now();

        let mut events = vec![RuntimeEvent::RouterStateChanged {
            intent: decision.intent,
            source: decision.source,
        }];

        if message.is_empty() {
            events.push(RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: format!(
                    "Intent set to {:?}. Waiting for your next message.",
                    decision.intent
                ),
            });
            self.save()?;
            return Ok(events);
        }

        self.append_transcript("user", message.to_string());
        self.snapshot.messages.push(Message {
            role: "user".to_string(),
            content: Some(message.to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        });

        let loop_events = self.run_model_loop().await?;
        events.extend(loop_events);
        self.save()?;
        Ok(events)
    }

    pub async fn submit_input_streaming(
        &mut self,
        input: &str,
        event_tx: mpsc::Sender<RuntimeEvent>,
    ) -> anyhow::Result<()> {
        if let Some(events) = self.handle_internal_command(input).await? {
            for event in events {
                let _ = event_tx.send(event);
            }
            self.save()?;
            return Ok(());
        }

        let (override_intent, body) = parse_slash_override(input);
        let message = body.trim();
        let has_plan = self.plan_manager.load().objective != "(none)";
        let decision = decide_intent(
            message,
            override_intent,
            has_plan,
            !self.lsp.diagnostics.is_empty(),
        );

        self.snapshot.metadata.router_intent = decision.intent;
        self.snapshot.metadata.last_active_at = Utc::now();

        let _ = event_tx.send(RuntimeEvent::RouterStateChanged {
            intent: decision.intent,
            source: decision.source,
        });

        if message.is_empty() {
            let _ = event_tx.send(RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: format!(
                    "Intent set to {:?}. Waiting for your next message.",
                    decision.intent
                ),
            });
            self.save()?;
            return Ok(());
        }

        self.append_transcript("user", message.to_string());
        self.snapshot.messages.push(Message {
            role: "user".to_string(),
            content: Some(message.to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        });

        self.run_model_loop_streaming(&event_tx).await?;
        self.save()?;
        Ok(())
    }

    pub async fn resolve_approval(
        &mut self,
        approval_id: &str,
        approved: bool,
    ) -> anyhow::Result<Vec<RuntimeEvent>> {
        let Some(index) = self
            .snapshot
            .approvals
            .iter()
            .position(|approval| approval.id == approval_id)
        else {
            return Ok(vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!("Approval {approval_id} not found."),
            }]);
        };

        let mut approval = self.snapshot.approvals[index].clone();
        approval.status = if approved {
            ApprovalStatus::Approved
        } else {
            ApprovalStatus::Denied
        };
        self.snapshot.approvals[index] = approval.clone();
        self.refresh_counts();

        let mut events = vec![RuntimeEvent::ApprovalResolved {
            approval: approval.clone(),
        }];

        if !approved {
            self.append_transcript(
                "system",
                format!("Denied approval for {}", approval.summary),
            );
            self.save()?;
            return Ok(events);
        }

        let Some(tool_call) = deserialize_approval_tool(&approval)? else {
            self.save()?;
            return Ok(events);
        };

        let execution = ToolExecution {
            tool_name: approval.tool_name.clone(),
            summary: approval.summary.clone(),
            result_preview: None,
        };
        events.push(RuntimeEvent::ToolCallStarted {
            execution: execution.clone(),
        });

        let result = execute_tool(&mut self.registry, &tool_call).await?;
        if let Some(tool_call_id) = approval.tool_call_id.clone() {
            self.snapshot.messages.push(Message {
                role: "tool".to_string(),
                content: Some(serde_json::to_string(&result)?),
                tool_calls: None,
                tool_call_id: Some(tool_call_id),
                reasoning: None,
                reasoning_details: None,
            });
        }
        self.append_transcript("tool", transcript_preview(&approval.tool_name, &result));
        events.push(RuntimeEvent::ToolCallFinished { execution, result });

        let continuation = self.run_model_loop().await?;
        events.extend(continuation);
        self.save()?;
        Ok(events)
    }

    fn initial_events(&self, lifecycle: SessionLifecycle) -> Vec<RuntimeEvent> {
        vec![
            RuntimeEvent::SessionLifecycle {
                session_id: self.snapshot.metadata.session_id.clone(),
                lifecycle,
                summary: self.snapshot.metadata.title.clone(),
            },
            RuntimeEvent::PreflightReady {
                preflight: self.preflight.clone(),
            },
            RuntimeEvent::DiagnosticsUpdated {
                lsp: self.lsp.clone(),
            },
            RuntimeEvent::McpStateUpdated {
                mcp: self.mcp.clone(),
            },
        ]
    }

    fn refresh_system_prompt(&mut self) {
        let system = self.prompt_assembler.assemble_session_system(
            &self.workspace_state,
            self.snapshot.metadata.router_intent,
            self.autonomy,
            &self.preflight,
            &self.lsp,
            &self.mcp,
        );
        self.snapshot.messages[0] = Message {
            role: "system".to_string(),
            content: Some(system),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        };
    }

    async fn run_model_loop(&mut self) -> anyhow::Result<Vec<RuntimeEvent>> {
        let mut events = Vec::new();

        for _ in 0..4 {
            self.refresh_system_prompt();
            let (response, _) = self
                .model
                .chat(ChatRequest {
                    model: self.model_name.clone(),
                    messages: self.snapshot.messages.clone(),
                    tools: Some(self.model.tool_schemas()),
                    tool_choice: Some("auto".to_string()),
                    temperature: Some(0.2),
                    max_tokens: Some(4000),
                    reasoning: None,
                    parallel_tool_calls: Some(true),
                    stream: Some(false),
                })
                .await?;

            if let Some(content) = response.content.clone() {
                if !content.trim().is_empty() {
                    self.append_transcript("assistant", content.clone());
                    events.push(RuntimeEvent::MessageDelta {
                        role: "assistant".to_string(),
                        content,
                    });
                }
            }

            let tool_call_ids = response
                .tool_calls
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|call| call.id)
                .collect::<Vec<_>>();
            let tool_calls = ToolParser::parse_tool_calls(&response);

            self.snapshot.messages.push(Message {
                role: "assistant".to_string(),
                content: response.content,
                tool_calls: response.tool_calls,
                tool_call_id: None,
                reasoning: response.reasoning,
                reasoning_details: response.reasoning_details,
            });

            if tool_calls.is_empty() {
                break;
            }

            for (index, call) in tool_calls.into_iter().enumerate() {
                let tool_name = tool_name(&call).to_string();
                let risk = risk_class(&call);
                let execution = ToolExecution {
                    tool_name: tool_name.clone(),
                    summary: serde_json::to_string(&call).unwrap_or_else(|_| tool_name.clone()),
                    result_preview: None,
                };

                if requires_approval(self.autonomy, risk.clone()) {
                    let approval = ApprovalRequest {
                        id: Uuid::new_v4().to_string(),
                        tool_name,
                        summary: execution.summary.clone(),
                        risk,
                        status: ApprovalStatus::Pending,
                        created_at: Utc::now(),
                        tool_arguments: Some(serialize_tool_call(&call)?),
                        tool_call_id: tool_call_ids.get(index).cloned(),
                    };
                    self.snapshot.approvals.push(approval.clone());
                    self.refresh_counts();
                    events.push(RuntimeEvent::ApprovalRequested { approval });
                    return Ok(events);
                }

                events.push(RuntimeEvent::ToolCallStarted {
                    execution: execution.clone(),
                });
                let result = execute_tool(&mut self.registry, &call).await?;

                if result
                    .metadata
                    .as_ref()
                    .and_then(|meta| meta.get("running"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    let command_id = result
                        .metadata
                        .as_ref()
                        .and_then(|meta| meta.get("command_id"))
                        .and_then(Value::as_str)
                        .unwrap_or("command")
                        .to_string();
                    let job = crate::runtime::types::BackgroundJob {
                        id: command_id.clone(),
                        title: format!("Command {}", command_id),
                        status: crate::runtime::types::BackgroundJobStatus::Running,
                        detail: result.output.clone(),
                    };
                    self.snapshot.background_jobs.push(job.clone());
                    self.refresh_counts();
                    events.push(RuntimeEvent::BackgroundJobUpdated { job });
                }

                if let Some(tool_call_id) = tool_call_ids.get(index).cloned() {
                    self.snapshot.messages.push(Message {
                        role: "tool".to_string(),
                        content: Some(serde_json::to_string(&result)?),
                        tool_calls: None,
                        tool_call_id: Some(tool_call_id),
                        reasoning: None,
                        reasoning_details: None,
                    });
                }
                self.append_transcript("tool", transcript_preview(&tool_name, &result));
                events.push(RuntimeEvent::ToolCallFinished { execution, result });
            }
        }

        Ok(events)
    }

    async fn run_model_loop_streaming(
        &mut self,
        event_tx: &mpsc::Sender<RuntimeEvent>,
    ) -> anyhow::Result<()> {
        for _ in 0..4 {
            self.refresh_system_prompt();

            let request = ChatRequest {
                model: self.model_name.clone(),
                messages: self.snapshot.messages.clone(),
                tools: Some(self.model.tool_schemas()),
                tool_choice: Some("auto".to_string()),
                temperature: Some(0.2),
                max_tokens: Some(4000),
                reasoning: None,
                parallel_tool_calls: Some(true),
                stream: Some(true),
            };

            let mut rx = match self.model.chat_stream(request.clone()).await {
                Ok(rx) => rx,
                Err(_) => {
                    return self.fallback_to_non_streaming(request, event_tx).await;
                }
            };

            let mut chunks: Vec<StreamChunk> = Vec::new();
            let mut accumulated_content = String::new();

            while let Some(result) = rx.recv().await {
                match result {
                    Ok(chunk) => {
                        for choice in &chunk.choices {
                            if let Some(ref content) = choice.delta.content {
                                if !content.is_empty() {
                                    accumulated_content.push_str(content);
                                    let _ = event_tx.send(RuntimeEvent::StreamDelta {
                                        role: "assistant".to_string(),
                                        content: content.clone(),
                                        model: chunk.model.clone(),
                                    });
                                }
                            }
                        }
                        chunks.push(chunk);
                    }
                    Err(e) => {
                        let _ = event_tx.send(RuntimeEvent::MessageDelta {
                            role: "system".to_string(),
                            content: format!("Stream chunk error: {e}"),
                        });
                        break;
                    }
                }
            }

            let _ = event_tx.send(RuntimeEvent::StreamDone {
                model: chunks.last().and_then(|c| c.model.clone()),
            });

            let response = match accumulate_stream_to_response(&chunks) {
                Ok(resp) => resp,
                Err(e) => {
                    let _ = event_tx.send(RuntimeEvent::MessageDelta {
                        role: "system".to_string(),
                        content: format!("Stream accumulation error: {e}"),
                    });
                    return Ok(());
                }
            };

            let choice = match response.choices.into_iter().next() {
                Some(c) => c,
                None => {
                    let _ = event_tx.send(RuntimeEvent::MessageDelta {
                        role: "system".to_string(),
                        content: "No response from model.".to_string(),
                    });
                    return Ok(());
                }
            };

            if let Some(ref content) = choice.message.content {
                if !content.trim().is_empty() {
                    self.append_transcript("assistant", content.clone());
                }
            }

            let tool_call_ids = choice
                .message
                .tool_calls
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|call| call.id)
                .collect::<Vec<_>>();
            let tool_calls = ToolParser::parse_tool_calls(&choice.message);

            self.snapshot.messages.push(Message {
                role: choice.message.role,
                content: choice.message.content,
                tool_calls: choice.message.tool_calls,
                tool_call_id: None,
                reasoning: choice.message.reasoning,
                reasoning_details: choice.message.reasoning_details,
            });

            if tool_calls.is_empty() {
                break;
            }

            for (index, call) in tool_calls.into_iter().enumerate() {
                let tool_name = tool_name(&call).to_string();
                let risk = risk_class(&call);
                let execution = ToolExecution {
                    tool_name: tool_name.clone(),
                    summary: serde_json::to_string(&call).unwrap_or_else(|_| tool_name.clone()),
                    result_preview: None,
                };

                if requires_approval(self.autonomy, risk.clone()) {
                    let approval = ApprovalRequest {
                        id: Uuid::new_v4().to_string(),
                        tool_name,
                        summary: execution.summary.clone(),
                        risk,
                        status: ApprovalStatus::Pending,
                        created_at: Utc::now(),
                        tool_arguments: Some(serialize_tool_call(&call)?),
                        tool_call_id: tool_call_ids.get(index).cloned(),
                    };
                    self.snapshot.approvals.push(approval.clone());
                    self.refresh_counts();
                    let _ = event_tx.send(RuntimeEvent::ApprovalRequested { approval });
                    return Ok(());
                }

                let _ = event_tx.send(RuntimeEvent::ToolCallStarted {
                    execution: execution.clone(),
                });
                let result = execute_tool(&mut self.registry, &call).await?;

                if result
                    .metadata
                    .as_ref()
                    .and_then(|meta| meta.get("running"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    let command_id = result
                        .metadata
                        .as_ref()
                        .and_then(|meta| meta.get("command_id"))
                        .and_then(Value::as_str)
                        .unwrap_or("command")
                        .to_string();
                    let job = crate::runtime::types::BackgroundJob {
                        id: command_id.clone(),
                        title: format!("Command {}", command_id),
                        status: crate::runtime::types::BackgroundJobStatus::Running,
                        detail: result.output.clone(),
                    };
                    self.snapshot.background_jobs.push(job.clone());
                    self.refresh_counts();
                    let _ = event_tx.send(RuntimeEvent::BackgroundJobUpdated { job });
                }

                if let Some(tool_call_id) = tool_call_ids.get(index).cloned() {
                    self.snapshot.messages.push(Message {
                        role: "tool".to_string(),
                        content: Some(serde_json::to_string(&result)?),
                        tool_calls: None,
                        tool_call_id: Some(tool_call_id),
                        reasoning: None,
                        reasoning_details: None,
                    });
                }
                self.append_transcript("tool", transcript_preview(&tool_name, &result));
                let _ = event_tx.send(RuntimeEvent::ToolCallFinished { execution, result });
            }
        }

        Ok(())
    }

    async fn fallback_to_non_streaming(
        &mut self,
        request: ChatRequest,
        event_tx: &mpsc::Sender<RuntimeEvent>,
    ) -> anyhow::Result<()> {
        let (response, _) = match self.model.chat(request).await {
            Ok(resp) => resp,
            Err(e) => {
                let _ = event_tx.send(RuntimeEvent::MessageDelta {
                    role: "system".to_string(),
                    content: format!("Model error: {e}"),
                });
                return Ok(());
            }
        };

        if let Some(content) = response.content.clone() {
            if !content.trim().is_empty() {
                let _ = event_tx.send(RuntimeEvent::MessageDelta {
                    role: "assistant".to_string(),
                    content: content.clone(),
                });
                self.append_transcript("assistant", content);
            }
        }

        let tool_call_ids = response
            .tool_calls
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|call| call.id)
            .collect::<Vec<_>>();
        let tool_calls = ToolParser::parse_tool_calls(&response);

        self.snapshot.messages.push(Message {
            role: "assistant".to_string(),
            content: response.content,
            tool_calls: response.tool_calls,
            tool_call_id: None,
            reasoning: response.reasoning,
            reasoning_details: response.reasoning_details,
        });

        if !tool_calls.is_empty() {
            for (index, call) in tool_calls.into_iter().enumerate() {
                let tool_name = tool_name(&call).to_string();
                let risk = risk_class(&call);
                let execution = ToolExecution {
                    tool_name: tool_name.clone(),
                    summary: serde_json::to_string(&call).unwrap_or_else(|_| tool_name.clone()),
                    result_preview: None,
                };

                if requires_approval(self.autonomy, risk.clone()) {
                    let approval = ApprovalRequest {
                        id: Uuid::new_v4().to_string(),
                        tool_name,
                        summary: execution.summary.clone(),
                        risk,
                        status: ApprovalStatus::Pending,
                        created_at: Utc::now(),
                        tool_arguments: Some(serialize_tool_call(&call)?),
                        tool_call_id: tool_call_ids.get(index).cloned(),
                    };
                    self.snapshot.approvals.push(approval.clone());
                    self.refresh_counts();
                    let _ = event_tx.send(RuntimeEvent::ApprovalRequested { approval });
                    return Ok(());
                }

                let _ = event_tx.send(RuntimeEvent::ToolCallStarted {
                    execution: execution.clone(),
                });
                let result = execute_tool(&mut self.registry, &call).await?;

                if let Some(tool_call_id) = tool_call_ids.get(index).cloned() {
                    self.snapshot.messages.push(Message {
                        role: "tool".to_string(),
                        content: Some(serde_json::to_string(&result)?),
                        tool_calls: None,
                        tool_call_id: Some(tool_call_id),
                        reasoning: None,
                        reasoning_details: None,
                    });
                }
                self.append_transcript("tool", transcript_preview(&tool_name, &result));
                let _ = event_tx.send(RuntimeEvent::ToolCallFinished { execution, result });
            }
        }

        Ok(())
    }

    async fn handle_internal_command(
        &mut self,
        input: &str,
    ) -> anyhow::Result<Option<Vec<RuntimeEvent>>> {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return Ok(None);
        }

        let parts = trimmed.split_whitespace().collect::<Vec<_>>();
        if parts.is_empty() {
            return Ok(None);
        }

        if let Some(command) = trimmed.strip_prefix("/mcp call ") {
            return Ok(Some(self.handle_mcp_call(command.trim()).await?));
        }

        match parts.as_slice() {
            ["/approvals"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: format!(
                    "Pending approvals: {}",
                    self.snapshot
                        .approvals
                        .iter()
                        .filter(|approval| approval.status == ApprovalStatus::Pending)
                        .map(|approval| format!("{} ({})", approval.id, approval.tool_name))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }])),
            ["/approvals", "approve", approval_id] => {
                Ok(Some(self.resolve_approval(approval_id, true).await?))
            }
            ["/approvals", "deny", approval_id] => {
                Ok(Some(self.resolve_approval(approval_id, false).await?))
            }
            ["/context", "add", path] => {
                let path = (*path).to_string();
                if !self.snapshot.composer.context_items.contains(&path) {
                    self.snapshot.composer.context_items.push(path.clone());
                }
                Ok(Some(vec![RuntimeEvent::MessageDelta {
                    role: "assistant".to_string(),
                    content: format!("Added context: {path}"),
                }]))
            }
            ["/context", "clear"] => {
                self.snapshot.composer.context_items.clear();
                Ok(Some(vec![RuntimeEvent::MessageDelta {
                    role: "assistant".to_string(),
                    content: "Cleared context items.".to_string(),
                }]))
            }
            ["/mcp"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: self.render_mcp_summary(),
            }])),
            ["/mcp", "refresh"] => Ok(Some(self.handle_mcp_refresh().await?)),
            ["/lsp"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: self.render_lsp_summary(),
            }])),
            ["/lsp", "refresh"] => Ok(Some(self.handle_lsp_refresh().await?)),
            ["/lsp", "diagnostics"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: self.render_lsp_diagnostics(),
            }])),
            ["/lsp", "symbols"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: self.render_lsp_symbols(),
            }])),
            _ => Ok(None),
        }
    }

    async fn handle_lsp_refresh(&mut self) -> anyhow::Result<Vec<RuntimeEvent>> {
        self.lsp = refresh_lsp_snapshot(&self.workspace_root).await?;
        self.refresh_system_prompt();
        Ok(vec![
            RuntimeEvent::DiagnosticsUpdated {
                lsp: self.lsp.clone(),
            },
            RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: self.render_lsp_summary(),
            },
        ])
    }

    async fn handle_mcp_refresh(&mut self) -> anyhow::Result<Vec<RuntimeEvent>> {
        self.mcp = refresh_mcp_snapshot(&self.workspace_root).await?;
        self.refresh_system_prompt();
        Ok(vec![
            RuntimeEvent::McpStateUpdated {
                mcp: self.mcp.clone(),
            },
            RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: self.render_mcp_summary(),
            },
        ])
    }

    async fn handle_mcp_call(&mut self, command: &str) -> anyhow::Result<Vec<RuntimeEvent>> {
        let mut parts = command.splitn(3, ' ');
        let Some(server_name) = parts.next().filter(|item| !item.is_empty()) else {
            return Ok(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: "Usage: /mcp call <server> <tool> [json]".to_string(),
            }]);
        };
        let Some(tool_name) = parts.next().filter(|item| !item.is_empty()) else {
            return Ok(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: "Usage: /mcp call <server> <tool> [json]".to_string(),
            }]);
        };
        let arguments = parts.next().map(str::trim).filter(|item| !item.is_empty());
        let arguments = match arguments {
            Some(raw) => match serde_json::from_str::<Value>(raw) {
                Ok(value) => value,
                Err(error) => {
                    return Ok(vec![RuntimeEvent::MessageDelta {
                        role: "assistant".to_string(),
                        content: format!("Invalid MCP arguments JSON: {error}"),
                    }]);
                }
            },
            None => Value::Object(Default::default()),
        };

        let execution = ToolExecution {
            tool_name: format!("mcp:{}:{}", server_name, tool_name),
            summary: format!("{server_name}:{tool_name} {}", arguments),
            result_preview: None,
        };
        let mut events = vec![RuntimeEvent::ToolCallStarted {
            execution: execution.clone(),
        }];
        let result = call_mcp_tool(&self.workspace_root, server_name, tool_name, arguments).await?;
        self.append_transcript("tool", transcript_preview(&execution.tool_name, &result));
        events.push(RuntimeEvent::ToolCallFinished {
            execution,
            result: result.clone(),
        });
        events.push(RuntimeEvent::MessageDelta {
            role: "assistant".to_string(),
            content: result.output.clone(),
        });
        Ok(events)
    }

    fn render_lsp_summary(&self) -> String {
        let servers = if self.lsp.servers.is_empty() {
            "none".to_string()
        } else {
            self.lsp
                .servers
                .iter()
                .map(|server| format!("{}:{}:{}", server.language, server.command, server.ready))
                .collect::<Vec<_>>()
                .join(", ")
        };

        format!(
            "LSP ready={} roots={} diagnostics={} servers={} jumps={}",
            self.lsp.ready,
            if self.lsp.active_roots.is_empty() {
                "none".to_string()
            } else {
                self.lsp.active_roots.join(", ")
            },
            self.lsp.diagnostics.len(),
            servers,
            self.lsp.symbol_jumps.len()
        )
    }

    fn render_lsp_diagnostics(&self) -> String {
        if self.lsp.diagnostics.is_empty() {
            return "No cached diagnostics.".to_string();
        }

        format!(
            "Diagnostics: {}",
            self.lsp
                .diagnostics
                .iter()
                .take(8)
                .map(|diagnostic| format!("{} {}", diagnostic.path, diagnostic.message))
                .collect::<Vec<_>>()
                .join(" | ")
        )
    }

    fn render_lsp_symbols(&self) -> String {
        if self.lsp.symbol_jumps.is_empty() {
            return "No symbol jumps available.".to_string();
        }

        format!(
            "Symbols: {}",
            self.lsp
                .symbol_jumps
                .iter()
                .take(8)
                .map(|jump| format!("{}@{}:{}", jump.name, jump.file_path, jump.line))
                .collect::<Vec<_>>()
                .join(" | ")
        )
    }

    fn render_mcp_summary(&self) -> String {
        let servers = if self.mcp.servers.is_empty() {
            "none".to_string()
        } else {
            self.mcp
                .servers
                .iter()
                .map(|server| {
                    let error = server
                        .last_error
                        .as_ref()
                        .map(|item| format!(" err={item}"))
                        .unwrap_or_default();
                    format!(
                        "{}:{:?}:tools={}({}){}",
                        server.name, server.status, server.tool_count, server.approval_mode, error
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };

        format!(
            "MCP ready={} servers={} inventory={}",
            self.mcp.ready,
            servers,
            if self.mcp.tools.is_empty() {
                "none".to_string()
            } else {
                self.mcp.tools.join(", ")
            }
        )
    }

    fn append_transcript(&mut self, role: &str, content: String) {
        self.snapshot.transcript.push(TranscriptEntry {
            role: role.to_string(),
            content,
            timestamp: Utc::now(),
        });
    }

    fn refresh_counts(&mut self) {
        self.snapshot.metadata.pending_approvals = self
            .snapshot
            .approvals
            .iter()
            .filter(|approval| approval.status == ApprovalStatus::Pending)
            .count();
        self.snapshot.metadata.background_jobs = self.snapshot.background_jobs.len();
        self.snapshot.metadata.last_active_at = Utc::now();
    }

    fn save(&self) -> anyhow::Result<()> {
        self.store.save_snapshot(&self.snapshot)
    }
}

fn new_session_snapshot(workspace_root: &Path, prompt: Option<String>) -> SessionSnapshot {
    let now = Utc::now();
    SessionSnapshot {
        metadata: SessionMetadata {
            session_id: Uuid::new_v4().to_string(),
            workspace_root: workspace_root.display().to_string(),
            title: prompt.unwrap_or_else(|| "Interactive session".to_string()),
            status: SessionStatus::Active,
            created_at: now,
            last_active_at: now,
            router_intent: RouterIntent::Explore,
            pending_approvals: 0,
            background_jobs: 0,
        },
        transcript: Vec::new(),
        messages: Vec::new(),
        approvals: Vec::new(),
        background_jobs: Vec::new(),
        preflight: WorkspacePreflight::default(),
        composer: Default::default(),
    }
}

async fn execute_tool(registry: &mut ToolRegistry, call: &ToolCall) -> anyhow::Result<ToolResult> {
    registry
        .execute(tool_name(call), serde_json::to_value(call)?)
        .await
}

fn tool_name(call: &ToolCall) -> &'static str {
    match call {
        ToolCall::ReadRange { .. } => "read_range",
        ToolCall::ReadSymbol { .. } => "read_symbol",
        ToolCall::GrepSearch { .. } => "grep_search",
        ToolCall::GlobSearch { .. } => "glob_search",
        ToolCall::ListDir { .. } => "list_dir",
        ToolCall::SemanticSearch { .. } => "semantic_search",
        ToolCall::ParallelSearch { .. } => "parallel_search",
        ToolCall::EditPatch { .. } => "edit_patch",
        ToolCall::WriteFile { .. } => "write_file",
        ToolCall::RunCommand { .. } => "run_command",
        ToolCall::PollCommand { .. } => "poll_command",
        ToolCall::PlanUpdate { .. } => "plan_update",
        ToolCall::CheckpointCreate { .. } => "checkpoint_create",
        ToolCall::CheckpointRestore { .. } => "checkpoint_restore",
        ToolCall::MemoryStage { .. } => "memory_stage",
        ToolCall::MemoryCommit { .. } => "memory_commit",
    }
}

fn risk_class(call: &ToolCall) -> RiskClass {
    match call {
        ToolCall::RunCommand { risk_class, .. } => risk_class.clone(),
        _ => RiskClass::SafeExec,
    }
}

fn serialize_tool_call(call: &ToolCall) -> anyhow::Result<String> {
    Ok(serde_json::to_string(call)?)
}

fn deserialize_approval_tool(approval: &ApprovalRequest) -> anyhow::Result<Option<ToolCall>> {
    let Some(raw) = approval.tool_arguments.as_ref() else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_str(raw)?))
}

fn transcript_preview(tool_name: &str, result: &ToolResult) -> String {
    let preview = result.output.lines().next().unwrap_or("ok");
    format!("{tool_name}: {preview}")
}

fn detect_workspace(root: &Path) -> anyhow::Result<WorkspaceState> {
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(root)
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_else(|| "unknown".to_string())
        .trim()
        .to_string();

    let dirty_files = std::process::Command::new("git")
        .args(["status", "--short"])
        .current_dir(root)
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();

    Ok(WorkspaceState {
        root_path: root.display().to_string(),
        branch,
        dirty_files,
        open_files: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::store::IndexStore;
    use crate::indexer::types::{Index, Symbol};
    use crate::providers::types::{FunctionCall, ToolCallBlock};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    struct FakeModel {
        replies: std::sync::Mutex<Vec<Message>>,
        tools: Vec<ToolSchema>,
    }

    #[async_trait]
    impl RuntimeModel for FakeModel {
        async fn chat(&self, _request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)> {
            let mut replies = self.replies.lock().unwrap();
            Ok((replies.remove(0), None))
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamChunk>>> {
            Err(anyhow::anyhow!("FakeModel does not support streaming"))
        }

        fn tool_schemas(&self) -> Vec<ToolSchema> {
            self.tools.clone()
        }
    }

    fn fake_model(replies: Vec<Message>) -> Arc<dyn RuntimeModel> {
        Arc::new(FakeModel {
            replies: std::sync::Mutex::new(replies),
            tools: crate::providers::types::default_tool_schemas(),
        })
    }

    #[tokio::test]
    async fn bootstrap_emits_preflight_lsp_and_mcp_events() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='demo'\nversion='0.1.0'\n",
        )
        .unwrap();

        let (_runtime, events) = SessionRuntime::bootstrap(
            dir.path(),
            "demo-model".to_string(),
            "openrouter".to_string(),
            InteractiveRequest {
                prompt: None,
                new_session: true,
                continue_last: false,
                session_id: None,
            },
            fake_model(Vec::new()),
        )
        .await
        .unwrap();

        assert!(matches!(
            events[0],
            RuntimeEvent::SessionLifecycle {
                lifecycle: SessionLifecycle::Started,
                ..
            }
        ));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::PreflightReady { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::DiagnosticsUpdated { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::McpStateUpdated { .. }))
        );
    }

    #[tokio::test]
    async fn slash_override_routes_turn_and_appends_transcript() {
        let dir = tempdir().unwrap();
        let model = fake_model(vec![Message {
            role: "assistant".to_string(),
            content: Some("Planning response".to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        }]);
        let (mut runtime, _) = SessionRuntime::bootstrap(
            dir.path(),
            "demo-model".to_string(),
            "openrouter".to_string(),
            InteractiveRequest {
                prompt: None,
                new_session: true,
                continue_last: false,
                session_id: None,
            },
            model,
        )
        .await
        .unwrap();

        let events = runtime
            .submit_input("/plan fix the architecture")
            .await
            .unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::RouterStateChanged {
                intent: RouterIntent::Plan,
                ..
            }
        )));
        assert_eq!(
            runtime.snapshot().metadata.router_intent,
            RouterIntent::Plan
        );
        assert_eq!(
            runtime.snapshot().transcript[0].content,
            "fix the architecture"
        );
    }

    #[tokio::test]
    async fn destructive_tool_calls_create_approval_queue() {
        let dir = tempdir().unwrap();
        let model = fake_model(vec![Message {
            role: "assistant".to_string(),
            content: Some("Need approval".to_string()),
            tool_calls: Some(vec![ToolCallBlock {
                id: "call-1".to_string(),
                r#type: "function".to_string(),
                function: FunctionCall {
                    name: "run_command".to_string(),
                    arguments: serde_json::json!({
                        "command": "rm -rf /tmp/demo",
                        "risk_class": "destructive"
                    })
                    .to_string(),
                },
            }]),
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        }]);
        let (mut runtime, _) = SessionRuntime::bootstrap(
            dir.path(),
            "demo-model".to_string(),
            "openrouter".to_string(),
            InteractiveRequest {
                prompt: None,
                new_session: true,
                continue_last: false,
                session_id: None,
            },
            model,
        )
        .await
        .unwrap();

        let events = runtime.submit_input("delete the temp tree").await.unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::ApprovalRequested { .. }))
        );
        assert_eq!(runtime.snapshot().metadata.pending_approvals, 1);
    }

    #[tokio::test]
    async fn detailed_lsp_commands_render_cached_diagnostics_and_symbols() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='demo'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join(".charm")).unwrap();
        fs::write(
            dir.path().join(".charm").join("diagnostics.json"),
            serde_json::to_string(&vec![crate::runtime::types::DiagnosticSummary {
                path: "src/main.rs".to_string(),
                message: "unused variable".to_string(),
            }])
            .unwrap(),
        )
        .unwrap();
        let mut index = Index::default();
        index.add_symbol(Symbol {
            name: "run_session".to_string(),
            kind: "function".to_string(),
            file_path: "src/main.rs".to_string(),
            line: 21,
            col: 1,
            signature: "fn run_session()".to_string(),
            docstring: None,
            body_start: 21,
            body_end: 40,
        });
        IndexStore::new(dir.path()).save(&index).unwrap();

        let (mut runtime, _) = SessionRuntime::bootstrap(
            dir.path(),
            "demo-model".to_string(),
            "openrouter".to_string(),
            InteractiveRequest {
                prompt: None,
                new_session: true,
                continue_last: false,
                session_id: None,
            },
            fake_model(Vec::new()),
        )
        .await
        .unwrap();

        let diagnostics = runtime.submit_input("/lsp diagnostics").await.unwrap();
        assert!(diagnostics.iter().any(|event| match event {
            RuntimeEvent::MessageDelta { content, .. } => content.contains("unused variable"),
            _ => false,
        }));

        let symbols = runtime.submit_input("/lsp symbols").await.unwrap();
        assert!(symbols.iter().any(|event| match event {
            RuntimeEvent::MessageDelta { content, .. } => content.contains("run_session"),
            _ => false,
        }));
    }

    #[tokio::test]
    async fn mcp_refresh_and_call_commands_use_runtime_registry() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".charm").join("mcp")).unwrap();
        let script = dir.path().join("fake-mcp.sh");
        fs::write(
            &script,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) echo '{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"capabilities\":{\"tools\":{\"listChanged\":true}},\"protocolVersion\":\"2025-03-26\",\"serverInfo\":{\"name\":\"workspace\",\"version\":\"0.1.0\"}}}' ;;\n    *'\"method\":\"notifications/initialized\"'*) : ;;\n    *'\"method\":\"tools/list\"'*) echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[{\"name\":\"echo\",\"description\":\"Echo\",\"inputSchema\":{\"type\":\"object\"}}]}}' ;;\n    *'\"method\":\"tools/call\"'*) echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hello from mcp\"}],\"isError\":false}}' ;;\n  esac\ndone\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        fs::write(
            dir.path().join(".charm").join("mcp").join("servers.json"),
            serde_json::json!({
                "servers": [
                    {
                        "name": "workspace",
                        "command": script,
                        "transport": "newline"
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let (mut runtime, _) = SessionRuntime::bootstrap(
            dir.path(),
            "demo-model".to_string(),
            "openrouter".to_string(),
            InteractiveRequest {
                prompt: None,
                new_session: true,
                continue_last: false,
                session_id: None,
            },
            fake_model(Vec::new()),
        )
        .await
        .unwrap();

        let refresh_events = runtime.submit_input("/mcp refresh").await.unwrap();
        assert!(
            refresh_events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::McpStateUpdated { .. }))
        );

        let call_events = runtime
            .submit_input("/mcp call workspace echo {\"value\":\"hi\"}")
            .await
            .unwrap();
        assert!(
            call_events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::ToolCallFinished { .. }))
        );
        assert!(call_events.iter().any(|event| match event {
            RuntimeEvent::MessageDelta { content, .. } => content.contains("hello from mcp"),
            _ => false,
        }));
    }

    struct FakeStreamingModel {
        chunks: std::sync::Mutex<Vec<StreamChunk>>,
        fallback_message: Option<Message>,
    }

    #[async_trait]
    impl RuntimeModel for FakeStreamingModel {
        async fn chat(&self, _request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)> {
            let msg = self.fallback_message.clone().unwrap_or_else(|| Message {
                role: "assistant".to_string(),
                content: Some("Fallback response".to_string()),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            });
            Ok((msg, None))
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamChunk>>> {
            let chunks = self.chunks.lock().unwrap().clone();
            let (tx, rx) = tokio::sync::mpsc::channel(10);
            tokio::spawn(async move {
                for chunk in chunks {
                    let _ = tx.send(Ok(chunk)).await;
                }
            });
            Ok(rx)
        }

        fn tool_schemas(&self) -> Vec<ToolSchema> {
            crate::providers::types::default_tool_schemas()
        }
    }

    fn streaming_model(chunks: Vec<StreamChunk>) -> Arc<dyn RuntimeModel> {
        Arc::new(FakeStreamingModel {
            chunks: std::sync::Mutex::new(chunks),
            fallback_message: None,
        })
    }

    fn streaming_model_with_fallback(
        chunks: Vec<StreamChunk>,
        fallback: Message,
    ) -> Arc<dyn RuntimeModel> {
        Arc::new(FakeStreamingModel {
            chunks: std::sync::Mutex::new(chunks),
            fallback_message: Some(fallback),
        })
    }

    struct FakeNonStreamingModel {
        message: Message,
    }

    #[async_trait]
    impl RuntimeModel for FakeNonStreamingModel {
        async fn chat(&self, _request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)> {
            Ok((self.message.clone(), None))
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamChunk>>> {
            Err(anyhow::anyhow!("Non-streaming model"))
        }

        fn tool_schemas(&self) -> Vec<ToolSchema> {
            crate::providers::types::default_tool_schemas()
        }
    }

    fn fake_model_no_stream(message: Message) -> Arc<dyn RuntimeModel> {
        Arc::new(FakeNonStreamingModel { message })
    }

    #[tokio::test]
    async fn streaming_path_emits_stream_delta_and_done() {
        let dir = tempdir().unwrap();
        let chunks = vec![
            StreamChunk {
                id: Some("test-1".to_string()),
                object: None,
                created: None,
                model: Some("gpt-4".to_string()),
                choices: vec![crate::providers::sse::StreamChoice {
                    index: 0,
                    delta: crate::providers::sse::StreamDelta {
                        role: Some("assistant".to_string()),
                        content: Some("Hello".to_string()),
                        tool_calls: None,
                    },
                    finish_reason: None,
                }],
                usage: None,
            },
            StreamChunk {
                id: Some("test-1".to_string()),
                object: None,
                created: None,
                model: Some("gpt-4".to_string()),
                choices: vec![crate::providers::sse::StreamChoice {
                    index: 0,
                    delta: crate::providers::sse::StreamDelta {
                        role: None,
                        content: Some(" world".to_string()),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: None,
            },
        ];

        let (mut runtime, _) = SessionRuntime::bootstrap(
            dir.path(),
            "demo-model".to_string(),
            "openrouter".to_string(),
            InteractiveRequest {
                prompt: Some("Test streaming".to_string()),
                new_session: true,
                continue_last: false,
                session_id: None,
            },
            streaming_model(chunks),
        )
        .await
        .unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        runtime.submit_input_streaming("Hello", tx).await.unwrap();

        let events: Vec<RuntimeEvent> = rx.try_iter().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::StreamDelta { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::StreamDone { .. }))
        );

        let deltas: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                RuntimeEvent::StreamDelta { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["Hello", " world"]);
    }

    #[tokio::test]
    async fn streaming_fallback_to_non_streaming_when_unsupported() {
        let dir = tempdir().unwrap();
        let fallback = Message {
            role: "assistant".to_string(),
            content: Some("Non-streaming response".to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        };

        let (mut runtime, _) = SessionRuntime::bootstrap(
            dir.path(),
            "demo-model".to_string(),
            "openrouter".to_string(),
            InteractiveRequest {
                prompt: Some("Test fallback".to_string()),
                new_session: true,
                continue_last: false,
                session_id: None,
            },
            fake_model_no_stream(fallback),
        )
        .await
        .unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        runtime.submit_input_streaming("Hello", tx).await.unwrap();

        let events: Vec<RuntimeEvent> = rx.try_iter().collect();
        assert!(
            events.iter().any(
                |e| matches!(e, RuntimeEvent::MessageDelta { role, .. } if role == "assistant")
            )
        );
    }

    #[tokio::test]
    async fn streaming_with_tool_calls_accumulates_correctly() {
        let dir = tempdir().unwrap();
        let chunks = vec![
            StreamChunk {
                id: Some("test-1".to_string()),
                object: None,
                created: None,
                model: Some("gpt-4".to_string()),
                choices: vec![crate::providers::sse::StreamChoice {
                    index: 0,
                    delta: crate::providers::sse::StreamDelta {
                        role: Some("assistant".to_string()),
                        content: Some("I'll help".to_string()),
                        tool_calls: None,
                    },
                    finish_reason: None,
                }],
                usage: None,
            },
            StreamChunk {
                id: Some("test-1".to_string()),
                object: None,
                created: None,
                model: Some("gpt-4".to_string()),
                choices: vec![crate::providers::sse::StreamChoice {
                    index: 0,
                    delta: crate::providers::sse::StreamDelta {
                        role: None,
                        content: None,
                        tool_calls: Some(vec![crate::providers::sse::StreamToolCall {
                            index: 0,
                            id: Some("call-1".to_string()),
                            call_type: Some("function".to_string()),
                            function: Some(crate::providers::sse::StreamFunction {
                                name: Some("list_dir".to_string()),
                                arguments: Some("{\"dir_path\": \".\"}".to_string()),
                            }),
                        }]),
                    },
                    finish_reason: Some("tool_calls".to_string()),
                }],
                usage: None,
            },
        ];

        let (mut runtime, _) = SessionRuntime::bootstrap(
            dir.path(),
            "demo-model".to_string(),
            "openrouter".to_string(),
            InteractiveRequest {
                prompt: Some("Test tool streaming".to_string()),
                new_session: true,
                continue_last: false,
                session_id: None,
            },
            streaming_model(chunks),
        )
        .await
        .unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        runtime
            .submit_input_streaming("List files", tx)
            .await
            .unwrap();

        let events: Vec<RuntimeEvent> = rx.try_iter().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::StreamDelta { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::StreamDone { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::ToolCallStarted { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::ToolCallFinished { .. }))
        );
    }
}
