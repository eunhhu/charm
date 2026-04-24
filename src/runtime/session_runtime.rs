use super::mcp::{call_mcp_tool, discover_mcp_tools, refresh_mcp_snapshot};
use super::router::{decide_intent, parse_slash_override, requires_tool_approval, tool_risk};
use super::subagent::{SubAgentBus, SubAgentReport, spawn_executor_subagent};
use super::types::{
    ApprovalRequest, ApprovalStatus, AutonomyLevel, BackgroundJob, BackgroundJobKind,
    BackgroundJobStatus, LspSnapshot, McpSnapshot, RouterIntent, RuntimeEvent, SessionLifecycle,
    ToolExecution, WorkspacePreflight,
};
use super::workspace::{build_preflight, collect_lsp_snapshot, refresh_lsp_snapshot};
use crate::agent::context_compressor::ContextCompressor;
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
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use uuid::Uuid;

/// Maximum transcript entries retained. Older entries are trimmed on save.
const MAX_TRANSCRIPT_ENTRIES: usize = 500;
/// Maximum messages retained (including system prompt). Older messages are
/// trimmed on save, with orphaned tool responses removed for consistency.
const MAX_MESSAGES: usize = 128;
/// Maximum resolved (approved/denied) approvals retained. Pending approvals
/// are never trimmed.
const MAX_RESOLVED_APPROVALS: usize = 20;
/// Maximum completed/failed/cancelled background jobs retained. Running and
/// queued jobs are never trimmed.
const MAX_COMPLETED_JOBS: usize = 20;

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
    default_model_name: String,
    model_display: String,
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
    subagent_bus: SubAgentBus,
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
        let workspace_state = crate::core::detect_workspace(workspace_root)?;
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

        let restored_autonomy = snapshot.metadata.autonomy_level;
        let pinned_model = snapshot.metadata.pinned_model.clone();
        let effective_model = pinned_model.clone().unwrap_or_else(|| model_name.clone());

        let mut runtime = Self {
            workspace_root: workspace_root.to_path_buf(),
            workspace_state,
            model_name: effective_model,
            default_model_name: model_name.clone(),
            model_display: pinned_model.unwrap_or(model_name),
            autonomy: restored_autonomy,
            store,
            plan_manager: PlanManager::new(workspace_root),
            registry: ToolRegistry::new(workspace_root),
            prompt_assembler: PromptAssembler::new(workspace_root).with_provider(&provider_hint),
            model,
            snapshot,
            preflight,
            lsp,
            mcp,
            subagent_bus: SubAgentBus::new(),
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
        let mut events = self.drain_background_events();
        if let Some(command_events) = self.handle_internal_command(input).await? {
            events.extend(command_events);
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

        events.push(RuntimeEvent::RouterStateChanged {
            intent: decision.intent,
            source: decision.source,
        });

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
        for event in self.drain_background_events() {
            let _ = event_tx.send(event);
        }
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

        let result = execute_tool_graceful(&mut self.registry, &tool_call).await;
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
            let (response, _) = match self
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
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    let msg = format!("Model request failed: {e}");
                    self.append_transcript("system", msg.clone());
                    events.push(RuntimeEvent::MessageDelta {
                        role: "system".to_string(),
                        content: msg,
                    });
                    return Ok(events);
                }
            };

            if let Some(content) = response.content.clone() {
                if !content.trim().is_empty() {
                    self.append_transcript("assistant", content.clone());
                    events.push(RuntimeEvent::MessageDelta {
                        role: "assistant".to_string(),
                        content,
                    });
                }
            }

            let all_tool_call_ids: Vec<String> = response
                .tool_calls
                .as_ref()
                .map(|tcs| tcs.iter().map(|tc| tc.id.clone()).collect())
                .unwrap_or_default();
            let parsed_calls = ToolParser::parse_tool_calls_with_ids(&response);

            self.snapshot.messages.push(Message {
                role: "assistant".to_string(),
                content: response.content,
                tool_calls: response.tool_calls,
                tool_call_id: None,
                reasoning: response.reasoning,
                reasoning_details: response.reasoning_details,
            });

            // Emit error tool results for any raw tool_call_ids skipped by parsing
            let parsed_ids: HashSet<String> = parsed_calls.iter().map(|p| p.id.clone()).collect();
            for id in &all_tool_call_ids {
                if !parsed_ids.contains(id) {
                    self.snapshot.messages.push(Message {
                        role: "tool".to_string(),
                        content: Some(serde_json::to_string(&ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("Tool call parsing failed".to_string()),
                            metadata: None,
                        })?),
                        tool_calls: None,
                        tool_call_id: Some(id.clone()),
                        reasoning: None,
                        reasoning_details: None,
                    });
                }
            }

            if parsed_calls.is_empty() {
                break;
            }

            for parsed in parsed_calls {
                let id = parsed.id;
                let call = parsed.call;
                let tool_name = tool_name(&call).to_string();
                let risk = tool_risk(&call);
                let execution = ToolExecution {
                    tool_name: tool_name.clone(),
                    summary: serde_json::to_string(&call).unwrap_or_else(|_| tool_name.clone()),
                    result_preview: None,
                };

                if requires_tool_approval(self.autonomy, &call) {
                    let approval = ApprovalRequest {
                        id: Uuid::new_v4().to_string(),
                        tool_name,
                        summary: execution.summary.clone(),
                        risk,
                        status: ApprovalStatus::Pending,
                        created_at: Utc::now(),
                        tool_arguments: Some(serialize_tool_call(&call)?),
                        tool_call_id: Some(id),
                    };
                    self.snapshot.approvals.push(approval.clone());
                    self.refresh_counts();
                    events.push(RuntimeEvent::ApprovalRequested { approval });
                    return Ok(events);
                }

                if let Some(warn) = self.yolo_bypass_event(&tool_name, &risk) {
                    events.push(warn);
                }

                events.push(RuntimeEvent::ToolCallStarted {
                    execution: execution.clone(),
                });
                let result = execute_tool_graceful(&mut self.registry, &call).await;

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
                        kind: crate::runtime::types::BackgroundJobKind::Command,
                        progress: None,
                        metadata: None,
                    };
                    self.snapshot.background_jobs.push(job.clone());
                    self.refresh_counts();
                    events.push(RuntimeEvent::BackgroundJobUpdated { job });
                }

                self.snapshot.messages.push(Message {
                    role: "tool".to_string(),
                    content: Some(serde_json::to_string(&result)?),
                    tool_calls: None,
                    tool_call_id: Some(id),
                    reasoning: None,
                    reasoning_details: None,
                });
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
                Err(e) => {
                    let _ = event_tx.send(RuntimeEvent::MessageDelta {
                        role: "system".to_string(),
                        content: format!(
                            "Streaming unavailable ({e}), falling back to non-streaming."
                        ),
                    });
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

            let all_tool_call_ids: Vec<String> = choice
                .message
                .tool_calls
                .as_ref()
                .map(|tcs| tcs.iter().map(|tc| tc.id.clone()).collect())
                .unwrap_or_default();
            let parsed_calls = ToolParser::parse_tool_calls_with_ids(&choice.message);

            self.snapshot.messages.push(Message {
                role: choice.message.role,
                content: choice.message.content,
                tool_calls: choice.message.tool_calls,
                tool_call_id: None,
                reasoning: choice.message.reasoning,
                reasoning_details: choice.message.reasoning_details,
            });

            let parsed_ids: HashSet<String> = parsed_calls.iter().map(|p| p.id.clone()).collect();
            for id in &all_tool_call_ids {
                if !parsed_ids.contains(id) {
                    self.snapshot.messages.push(Message {
                        role: "tool".to_string(),
                        content: Some(serde_json::to_string(&ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("Tool call parsing failed".to_string()),
                            metadata: None,
                        })?),
                        tool_calls: None,
                        tool_call_id: Some(id.clone()),
                        reasoning: None,
                        reasoning_details: None,
                    });
                }
            }

            if parsed_calls.is_empty() {
                break;
            }

            for parsed in parsed_calls {
                let id = parsed.id;
                let call = parsed.call;
                let tool_name = tool_name(&call).to_string();
                let risk = tool_risk(&call);
                let execution = ToolExecution {
                    tool_name: tool_name.clone(),
                    summary: serde_json::to_string(&call).unwrap_or_else(|_| tool_name.clone()),
                    result_preview: None,
                };

                if requires_tool_approval(self.autonomy, &call) {
                    let approval = ApprovalRequest {
                        id: Uuid::new_v4().to_string(),
                        tool_name,
                        summary: execution.summary.clone(),
                        risk,
                        status: ApprovalStatus::Pending,
                        created_at: Utc::now(),
                        tool_arguments: Some(serialize_tool_call(&call)?),
                        tool_call_id: Some(id),
                    };
                    self.snapshot.approvals.push(approval.clone());
                    self.refresh_counts();
                    let _ = event_tx.send(RuntimeEvent::ApprovalRequested { approval });
                    return Ok(());
                }

                if let Some(warn) = self.yolo_bypass_event(&tool_name, &risk) {
                    let _ = event_tx.send(warn);
                }

                let _ = event_tx.send(RuntimeEvent::ToolCallStarted {
                    execution: execution.clone(),
                });
                let result = execute_tool_graceful(&mut self.registry, &call).await;

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
                        kind: crate::runtime::types::BackgroundJobKind::Command,
                        progress: None,
                        metadata: None,
                    };
                    self.snapshot.background_jobs.push(job.clone());
                    self.refresh_counts();
                    let _ = event_tx.send(RuntimeEvent::BackgroundJobUpdated { job });
                }

                self.snapshot.messages.push(Message {
                    role: "tool".to_string(),
                    content: Some(serde_json::to_string(&result)?),
                    tool_calls: None,
                    tool_call_id: Some(id),
                    reasoning: None,
                    reasoning_details: None,
                });
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

        let all_tool_call_ids: Vec<String> = response
            .tool_calls
            .as_ref()
            .map(|tcs| tcs.iter().map(|tc| tc.id.clone()).collect())
            .unwrap_or_default();
        let parsed_calls = ToolParser::parse_tool_calls_with_ids(&response);

        self.snapshot.messages.push(Message {
            role: "assistant".to_string(),
            content: response.content,
            tool_calls: response.tool_calls,
            tool_call_id: None,
            reasoning: response.reasoning,
            reasoning_details: response.reasoning_details,
        });

        let parsed_ids: HashSet<String> = parsed_calls.iter().map(|p| p.id.clone()).collect();
        for id in &all_tool_call_ids {
            if !parsed_ids.contains(id) {
                self.snapshot.messages.push(Message {
                    role: "tool".to_string(),
                    content: Some(serde_json::to_string(&ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Tool call parsing failed".to_string()),
                        metadata: None,
                    })?),
                    tool_calls: None,
                    tool_call_id: Some(id.clone()),
                    reasoning: None,
                    reasoning_details: None,
                });
            }
        }

        if !parsed_calls.is_empty() {
            for parsed in parsed_calls {
                let id = parsed.id;
                let call = parsed.call;
                let tool_name = tool_name(&call).to_string();
                let risk = tool_risk(&call);
                let execution = ToolExecution {
                    tool_name: tool_name.clone(),
                    summary: serde_json::to_string(&call).unwrap_or_else(|_| tool_name.clone()),
                    result_preview: None,
                };

                if requires_tool_approval(self.autonomy, &call) {
                    let approval = ApprovalRequest {
                        id: Uuid::new_v4().to_string(),
                        tool_name,
                        summary: execution.summary.clone(),
                        risk,
                        status: ApprovalStatus::Pending,
                        created_at: Utc::now(),
                        tool_arguments: Some(serialize_tool_call(&call)?),
                        tool_call_id: Some(id),
                    };
                    self.snapshot.approvals.push(approval.clone());
                    self.refresh_counts();
                    let _ = event_tx.send(RuntimeEvent::ApprovalRequested { approval });
                    return Ok(());
                }

                if let Some(warn) = self.yolo_bypass_event(&tool_name, &risk) {
                    let _ = event_tx.send(warn);
                }

                let _ = event_tx.send(RuntimeEvent::ToolCallStarted {
                    execution: execution.clone(),
                });
                let result = execute_tool_graceful(&mut self.registry, &call).await;

                self.snapshot.messages.push(Message {
                    role: "tool".to_string(),
                    content: Some(serde_json::to_string(&result)?),
                    tool_calls: None,
                    tool_call_id: Some(id),
                    reasoning: None,
                    reasoning_details: None,
                });
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

        if let Some(task) = trimmed.strip_prefix("/agent spawn ") {
            let task = task.trim();
            if task.is_empty() {
                return Ok(Some(vec![RuntimeEvent::MessageDelta {
                    role: "assistant".to_string(),
                    content: "Usage: /agent spawn <task description>".to_string(),
                }]));
            }
            return Ok(Some(self.spawn_subagent(task.to_string())));
        }

        match parts.as_slice() {
            ["/help"] | ["/?"] | ["/h"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: help_text(),
            }])),
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
            ["/autonomy"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: format!(
                    "Autonomy: {label} ({short}). Levels: conservative|balanced|aggressive|yolo. Change via /autonomy <level>.",
                    label = self.autonomy.label(),
                    short = self.autonomy.short()
                ),
            }])),
            ["/autonomy", level] => match AutonomyLevel::parse(level) {
                Some(parsed) => Ok(Some(self.set_autonomy(parsed, "slash"))),
                None => Ok(Some(vec![RuntimeEvent::MessageDelta {
                    role: "assistant".to_string(),
                    content: format!(
                        "Unknown autonomy level '{level}'. Use conservative|balanced|aggressive|yolo.",
                    ),
                }])),
            },
            ["/yolo"] => Ok(Some(self.set_autonomy(AutonomyLevel::Yolo, "slash"))),
            ["/safe"] => Ok(Some(self.set_autonomy(AutonomyLevel::Conservative, "slash"))),
            ["/compact"] => Ok(Some(self.compact_context())),
            ["/clear"] => Ok(Some(self.clear_transcript())),
            ["/new"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: "Start a new session with `charm --new` or Ctrl+N. The TUI will spin up a fresh session in a moment.".to_string(),
            }])),
            ["/session"] | ["/sessions"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: self.render_session_summary(),
            }])),
            ["/session", "list"] | ["/sessions", "list"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: self.render_session_summary(),
            }])),
            ["/session", "next"] | ["/sessions", "next"] => {
                Ok(Some(self.switch_session_relative(1).await?))
            }
            ["/session", "prev"] | ["/sessions", "prev"] => {
                Ok(Some(self.switch_session_relative(-1).await?))
            }
            ["/session", target] | ["/sessions", target] => {
                Ok(Some(self.switch_session_by_id(target).await?))
            }
            ["/model"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: format!(
                    "Model: {} (request: {}). Change via /model <id>.",
                    self.model_display, self.model_name
                ),
            }])),
            ["/model", target] => Ok(Some(self.set_model(target.to_string()))),
            ["/agent"] | ["/agents"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: self.render_agent_summary(),
            }])),
            ["/agent", "list"] | ["/agents", "list"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: self.render_agent_summary(),
            }])),
            ["/agent", "kill", id] | ["/agents", "kill", id] => {
                Ok(Some(self.cancel_subagent(id)))
            }
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

    /// When autonomy is YOLO and the call would normally require approval,
    /// record a loud ⚠ marker in the transcript. This satisfies the
    /// "trace-first" commitment in `docs/charm-strategy.md § Autonomy
    /// Profiles`: YOLO bypasses the approval gate but never the trace gate.
    fn yolo_bypass_event(&mut self, tool_name: &str, risk: &RiskClass) -> Option<RuntimeEvent> {
        if self.autonomy != AutonomyLevel::Yolo {
            return None;
        }
        if !matches!(risk, RiskClass::Destructive | RiskClass::ExternalSideEffect) {
            return None;
        }
        let msg = format!(
            "⚠ YOLO auto-approved {} ({:?}). Checkpoint your work before re-running.",
            tool_name, risk
        );
        self.append_transcript("system", msg.clone());
        Some(RuntimeEvent::MessageDelta {
            role: "system".to_string(),
            content: msg,
        })
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

    fn save(&mut self) -> anyhow::Result<()> {
        self.snapshot.trim_to_caps(
            MAX_TRANSCRIPT_ENTRIES,
            MAX_MESSAGES,
            MAX_RESOLVED_APPROVALS,
            MAX_COMPLETED_JOBS,
        );
        self.store.save_snapshot(&self.snapshot)
    }

    pub fn autonomy(&self) -> AutonomyLevel {
        self.autonomy
    }

    pub fn model_display(&self) -> &str {
        &self.model_display
    }

    pub fn subagent_bus(&self) -> SubAgentBus {
        self.subagent_bus.clone()
    }

    /// Drain any sub-agent updates that have been published by spawned tokio
    /// tasks and merge them into the snapshot + a batch of runtime events.
    pub fn drain_background_events(&mut self) -> Vec<RuntimeEvent> {
        let session_id = self.snapshot.metadata.session_id.clone();
        let pending = self.subagent_bus.drain_for_session(&session_id);
        if pending.is_empty() {
            return Vec::new();
        }
        let mut events = Vec::with_capacity(pending.len());
        for job in pending {
            self.apply_job_update(job.clone());
            events.push(RuntimeEvent::BackgroundJobUpdated { job });
        }
        self.refresh_counts();
        events
    }

    /// Poll background updates outside a user turn, merge them, and persist
    /// immediately. This keeps runtime state canonical even while the TUI is
    /// idle and no prompt is being submitted.
    pub fn poll_background_events(&mut self) -> anyhow::Result<Vec<RuntimeEvent>> {
        let events = self.drain_background_events();
        if !events.is_empty() {
            self.save()?;
        }
        Ok(events)
    }

    fn apply_job_update(&mut self, job: BackgroundJob) {
        if let Some(existing) = self
            .snapshot
            .background_jobs
            .iter_mut()
            .find(|item| item.id == job.id)
        {
            *existing = job;
        } else {
            self.snapshot.background_jobs.push(job);
        }
    }

    pub fn set_autonomy(&mut self, level: AutonomyLevel, source: &str) -> Vec<RuntimeEvent> {
        if self.autonomy == level {
            return vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!("Autonomy already {}.", level.label()),
            }];
        }
        self.autonomy = level;
        self.snapshot.metadata.autonomy_level = level;
        self.snapshot.metadata.last_active_at = Utc::now();
        self.refresh_system_prompt();
        self.append_transcript(
            "system",
            format!("Autonomy set to {} via {source}", level.label()),
        );
        vec![
            RuntimeEvent::AutonomyChanged {
                autonomy: level,
                source: source.to_string(),
            },
            RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!(
                    "Autonomy set to {label} ({short})",
                    label = level.label(),
                    short = level.short()
                ),
            },
        ]
    }

    pub fn cycle_autonomy(&mut self) -> Vec<RuntimeEvent> {
        self.set_autonomy(self.autonomy.cycle(), "hotkey")
    }

    pub fn set_model(&mut self, target: String) -> Vec<RuntimeEvent> {
        if target.trim().is_empty() {
            return vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: "Usage: /model <id>".to_string(),
            }];
        }
        self.model_name = target.clone();
        self.model_display = target.clone();
        self.snapshot.metadata.pinned_model = Some(target.clone());
        self.append_transcript(
            "system",
            format!("Model pinned to {target} for this session"),
        );
        vec![
            RuntimeEvent::ModelChanged {
                model: target.clone(),
                display: target.clone(),
            },
            RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!(
                    "Model set to {target}. (Note: TUI provider binding reuses current provider client.)"
                ),
            },
        ]
    }

    pub fn compact_context(&mut self) -> Vec<RuntimeEvent> {
        let before = self.snapshot.messages.len();
        // TODO(phase-3): replace this naive pass-through with a TokenSaver-backed
        // compactor. Right now we tell the ContextCompressor we are at 0/128K so
        // it only trims when the transcript is already huge. Per
        // `OPTIMIZATION_ROADMAP.md` Phase A 1.2 the real contract is:
        //   - preserve code spans verbatim (line numbers intact)
        //   - roll intermediate tool output + chat into a key-point summary
        //   - keep open questions, decisions, assumptions
        //   - drop duplicate tool calls
        // The TUI UX (/compact slash + Ctrl+K) already matches the final surface.
        ContextCompressor::compress(&mut self.snapshot.messages, 0, 128_000);
        let after = self.snapshot.messages.len();
        let removed = before.saturating_sub(after);
        let summary = if removed == 0 {
            "Context already compact.".to_string()
        } else {
            format!("Compacted context: {removed} messages rolled into summary.")
        };
        self.append_transcript("system", summary.clone());
        vec![
            RuntimeEvent::ContextCompacted {
                removed_messages: removed,
                summary: summary.clone(),
            },
            RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: summary,
            },
        ]
    }

    pub fn clear_transcript(&mut self) -> Vec<RuntimeEvent> {
        self.snapshot.transcript.clear();
        // Keep the system prompt in messages but drop everything else.
        self.snapshot
            .messages
            .retain(|message| message.role == "system");
        self.refresh_system_prompt();
        vec![RuntimeEvent::MessageDelta {
            role: "system".to_string(),
            content: "Transcript cleared. Conversation memory reset.".to_string(),
        }]
    }

    pub fn render_session_summary(&self) -> String {
        let entries = match self.store.list_metadata() {
            Ok(list) => list,
            Err(err) => return format!("Could not list sessions: {err}"),
        };
        if entries.is_empty() {
            return "No sessions saved yet.".to_string();
        }
        let active_id = &self.snapshot.metadata.session_id;
        let mut lines = vec![format!(
            "Sessions ({} total, current: {})",
            entries.len(),
            &active_id[..active_id.len().min(8)]
        )];
        for (idx, meta) in entries.iter().take(10).enumerate() {
            let marker = if &meta.session_id == active_id {
                "●"
            } else {
                "○"
            };
            lines.push(format!(
                "  {marker} [{idx}] {short} • {intent:?} • {title}",
                short = &meta.session_id[..meta.session_id.len().min(8)],
                intent = meta.router_intent,
                title = meta.title
            ));
        }
        lines.push(
            "Use /session next, /session prev, or /session <id-prefix> to switch.".to_string(),
        );
        lines.join("\n")
    }

    pub fn render_agent_summary(&self) -> String {
        let subagents: Vec<_> = self
            .snapshot
            .background_jobs
            .iter()
            .filter(|job| matches!(job.kind, BackgroundJobKind::SubAgent))
            .collect();
        if subagents.is_empty() {
            return "No sub-agents spawned. Use `/agent spawn <task>` to start one.".to_string();
        }
        let mut lines = vec![format!("Sub-agents ({} total)", subagents.len())];
        for job in subagents {
            let progress = job.progress.map(|p| format!(" {p}%")).unwrap_or_default();
            let icon = match job.status {
                BackgroundJobStatus::Queued => "⧗",
                BackgroundJobStatus::Running => "◉",
                BackgroundJobStatus::Completed => "✓",
                BackgroundJobStatus::Failed => "✗",
                BackgroundJobStatus::Cancelled => "⊘",
            };
            lines.push(format!(
                "  {icon} [{short}]{progress} {title} — {detail}",
                short = &job.id[..job.id.len().min(8)],
                title = job.title,
                detail = job.detail
            ));
        }
        lines.push("Use /agent kill <id-prefix> to cancel a sub-agent.".to_string());
        lines.join("\n")
    }

    pub fn spawn_subagent(&mut self, task: String) -> Vec<RuntimeEvent> {
        let model = self.model.clone();
        let model_name = self.model_name.clone();
        let workspace_root = self.workspace_root.clone();
        let job = spawn_executor_subagent(
            self.subagent_bus.clone(),
            self.snapshot.metadata.session_id.clone(),
            task.clone(),
            move |task| async move {
                run_isolated_subagent(model, model_name, workspace_root, task).await
            },
        );
        self.apply_job_update(job.clone());
        self.refresh_counts();
        self.append_transcript("system", format!("Sub-agent spawned for: {task}"));
        vec![
            RuntimeEvent::SubAgentSpawned {
                job_id: job.id.clone(),
                title: task.clone(),
            },
            RuntimeEvent::BackgroundJobUpdated { job },
            RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!("Sub-agent queued for: {task}. Check progress via /agent list."),
            },
        ]
    }

    pub fn cancel_subagent(&mut self, id_prefix: &str) -> Vec<RuntimeEvent> {
        let mut target_job = None;
        for job in self.snapshot.background_jobs.iter_mut() {
            if job.id.starts_with(id_prefix) {
                job.status = BackgroundJobStatus::Cancelled;
                job.detail = "Cancelled by user".to_string();
                target_job = Some(job.clone());
                break;
            }
        }
        self.refresh_counts();
        match target_job {
            Some(job) => vec![
                RuntimeEvent::BackgroundJobUpdated { job: job.clone() },
                RuntimeEvent::MessageDelta {
                    role: "system".to_string(),
                    content: format!(
                        "Cancelled sub-agent {} ({}).",
                        &job.id[..8.min(job.id.len())],
                        job.title
                    ),
                },
            ],
            None => vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!("No background job matches '{id_prefix}'."),
            }],
        }
    }

    pub async fn switch_session_by_id(
        &mut self,
        target: &str,
    ) -> anyhow::Result<Vec<RuntimeEvent>> {
        let all = self.store.list_metadata().unwrap_or_default();
        let matched = all
            .iter()
            .find(|meta| meta.session_id == target || meta.session_id.starts_with(target));
        let Some(meta) = matched else {
            return Ok(vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!("No session matches '{target}'."),
            }]);
        };
        if meta.session_id == self.snapshot.metadata.session_id {
            return Ok(vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: "Already on that session.".to_string(),
            }]);
        }
        let meta_clone = meta.clone();
        self.load_session_snapshot(&meta_clone.session_id).await
    }

    pub async fn switch_session_relative(
        &mut self,
        delta: i32,
    ) -> anyhow::Result<Vec<RuntimeEvent>> {
        let all = self.store.list_metadata().unwrap_or_default();
        if all.len() < 2 {
            return Ok(vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: "No other sessions to switch to.".to_string(),
            }]);
        }
        let current_idx = all
            .iter()
            .position(|meta| meta.session_id == self.snapshot.metadata.session_id)
            .unwrap_or(0);
        let len = all.len() as i32;
        let next_idx = ((current_idx as i32 + delta) % len + len) % len;
        let next_id = all[next_idx as usize].session_id.clone();
        self.load_session_snapshot(&next_id).await
    }

    async fn load_session_snapshot(
        &mut self,
        session_id: &str,
    ) -> anyhow::Result<Vec<RuntimeEvent>> {
        let loaded = self
            .store
            .load_snapshot(session_id)?
            .with_context(|| format!("missing session snapshot for {session_id}"))?;
        self.save()?;

        self.snapshot = loaded;
        if self.snapshot.messages.is_empty() || self.snapshot.messages[0].role != "system" {
            self.snapshot.messages.insert(
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
        self.autonomy = self.snapshot.metadata.autonomy_level;
        if let Some(pinned) = &self.snapshot.metadata.pinned_model {
            self.model_name = pinned.clone();
            self.model_display = pinned.clone();
        } else {
            self.model_name = self.default_model_name.clone();
            self.model_display = self.default_model_name.clone();
        }
        self.refresh_system_prompt();

        let session_id = self.snapshot.metadata.session_id.clone();
        let title = self.snapshot.metadata.title.clone();
        let intent = self.snapshot.metadata.router_intent;
        let autonomy = self.autonomy;
        self.save()?;

        Ok(vec![
            RuntimeEvent::SessionSwitched {
                session_id: session_id.clone(),
                title: title.clone(),
            },
            RuntimeEvent::SessionLifecycle {
                session_id,
                lifecycle: SessionLifecycle::Resumed,
                summary: title,
            },
            RuntimeEvent::RouterStateChanged {
                intent,
                source: "session_switch".to_string(),
            },
            RuntimeEvent::AutonomyChanged {
                autonomy,
                source: "session_switch".to_string(),
            },
        ])
    }
}

fn help_text() -> String {
    r#"Charm Help
──────────────────────────────────────────
Philosophy: evidence-first · tool-first · reference-first · trace-first
The agent picks the intent (explore/plan/build/verify) from your message —
you never have to toggle modes by hand. Autonomy profiles only control the
gate-bypass policy, not the routing.

Slash commands
  /help                  Show this help
  /explore|/plan|/build|/verify  Override intent for this turn only
  /autonomy <level>      conservative|balanced|aggressive|yolo
  /yolo                  Shortcut: /autonomy yolo (loud destructive trace)
  /safe                  Shortcut: /autonomy conservative
  /compact               Roll old turns into a summary (TokenSaver TODO)
  /clear                 Clear transcript (keep system prompt)
  /model <id>            Pin a model for this session
  /session [next|prev|<id>]  Rotate between sessions
  /agent spawn <task>    Start a background sub-agent
  /agent list|kill <id>  Inspect / cancel sub-agents
  /approvals             Show pending approvals
  /context add <path>    Pin a workspace context chip
  /context clear         Clear context chips
  /mcp / /lsp            Inspect MCP / LSP snapshots

Keyboard
  Ctrl+P          Command palette
  Ctrl+L          Session switcher (mouse + fuzzy search)
  Ctrl+M          Model switcher (provider-grouped, fuzzy)
  Ctrl+Shift+P    Provider connector
  Ctrl+Shift+M    MCP servers
  Ctrl+K          Skills / workflows
  Ctrl+N          New session
  Ctrl+Y          Cycle autonomy profile
  Ctrl+A          Sub-agent queue
  Ctrl+Tab        Next session
  Ctrl+Shift+Tab  Previous session
  Ctrl+B / Ctrl+D Toggle left / right docks
  Tab             Autocomplete slash command (completes common prefix)
  ↑ / ↓           Navigate slash dropdown / overlay list
  F1 / ?          Open this help overlay
  PgUp / PgDn     Scroll transcript page-by-page
  Shift+Up/Down   Fine-grain scroll (disengages auto-follow)
  Mouse wheel     Scroll transcript (wheel-down at bottom re-engages follow)
  Esc             Dismiss overlay / cancel

Autonomy levels (see docs/charm-strategy.md § Autonomy Profiles)
  conservative  Every write/exec needs approval. Reads auto.
  balanced      Reads + safe exec auto; stateful work asks.
  aggressive    Reads, searches, edits, tests auto; destructive asks.
  yolo          All tools auto-approved. Destructive ops still log a ⚠ trace
                line. Use git stash/checkpoint before risky runs.

Coming soon
  • Auto model routing via `charm delegate` (Planner/Worker).
  • TokenSaver-backed /compact with code-span preservation.
  • Sub-agent result review, merge, and cleanup workflow.
"#
    .to_string()
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
            autonomy_level: AutonomyLevel::Aggressive,
            pinned_model: None,
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

/// Like [`execute_tool`] but converts errors into a [`ToolResult`] with
/// `success: false` instead of propagating them via `?`.  This keeps the
/// model loop alive when a single tool fails — the model sees the error
/// in the tool-response message and can decide how to recover.
async fn execute_tool_graceful(registry: &mut ToolRegistry, call: &ToolCall) -> ToolResult {
    match execute_tool(registry, call).await {
        Ok(result) => result,
        Err(e) => ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("Tool execution failed: {e}")),
            metadata: None,
        },
    }
}

async fn run_isolated_subagent(
    model: Arc<dyn RuntimeModel>,
    model_name: String,
    workspace_root: PathBuf,
    task: String,
) -> anyhow::Result<SubAgentReport> {
    let worktree_path = prepare_subagent_worktree(&workspace_root)?;
    let mut registry = ToolRegistry::new(&worktree_path);
    let mut messages = vec![
        Message {
            role: "system".to_string(),
            content: Some(format!(
                "You are a background engineering sub-agent. Work only inside this isolated worktree: {}. \
                 Use tools when useful. Return a concise final summary and mention changed files.",
                worktree_path.display()
            )),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        },
        Message {
            role: "user".to_string(),
            content: Some(task),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        },
    ];
    let mut changed_files = BTreeSet::new();
    let mut summary = String::new();
    let mut turns = 0usize;

    for _ in 0..4 {
        turns += 1;
        let (response, _) = model
            .chat(ChatRequest {
                model: model_name.clone(),
                messages: messages.clone(),
                tools: Some(model.tool_schemas()),
                tool_choice: Some("auto".to_string()),
                temperature: Some(0.2),
                max_tokens: Some(2400),
                reasoning: None,
                parallel_tool_calls: Some(true),
                stream: Some(false),
            })
            .await?;

        if let Some(content) = response.content.clone()
            && !content.trim().is_empty()
        {
            summary = content;
        }

        let all_tool_call_ids: Vec<String> = response
            .tool_calls
            .as_ref()
            .map(|tcs| tcs.iter().map(|tc| tc.id.clone()).collect())
            .unwrap_or_default();
        let parsed_calls = ToolParser::parse_tool_calls_with_ids(&response);
        let parsed_ids: HashSet<String> = parsed_calls.iter().map(|p| p.id.clone()).collect();

        messages.push(Message {
            role: "assistant".to_string(),
            content: response.content,
            tool_calls: response.tool_calls,
            tool_call_id: None,
            reasoning: response.reasoning,
            reasoning_details: response.reasoning_details,
        });

        for id in &all_tool_call_ids {
            if !parsed_ids.contains(id) {
                messages.push(Message {
                    role: "tool".to_string(),
                    content: Some(serde_json::to_string(&ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Tool call parsing failed".to_string()),
                        metadata: None,
                    })?),
                    tool_calls: None,
                    tool_call_id: Some(id.clone()),
                    reasoning: None,
                    reasoning_details: None,
                });
            }
        }

        if parsed_calls.is_empty() {
            break;
        }

        for parsed in parsed_calls {
            let id = parsed.id;
            let call = parsed.call;
            let risk = tool_risk(&call);
            let result = if matches!(risk, RiskClass::Destructive | RiskClass::ExternalSideEffect) {
                ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Sub-agent blocked {:?} tool in isolated mode",
                        risk
                    )),
                    metadata: None,
                }
            } else {
                execute_tool_graceful(&mut registry, &call).await
            };
            collect_changed_files(&result, &worktree_path, &mut changed_files);
            messages.push(Message {
                role: "tool".to_string(),
                content: Some(serde_json::to_string(&result)?),
                tool_calls: None,
                tool_call_id: Some(id),
                reasoning: None,
                reasoning_details: None,
            });
        }
    }

    if summary.trim().is_empty() {
        summary = if changed_files.is_empty() {
            "Sub-agent completed without a text summary.".to_string()
        } else {
            format!(
                "Sub-agent completed. Changed files: {}",
                changed_files.iter().cloned().collect::<Vec<_>>().join(", ")
            )
        };
    }

    Ok(SubAgentReport {
        summary,
        worktree_path: Some(worktree_path.display().to_string()),
        changed_files: changed_files.into_iter().collect(),
        turns,
    })
}

fn prepare_subagent_worktree(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    let worktree_path = workspace_root
        .join(".charm")
        .join("worktrees")
        .join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(worktree_path.parent().unwrap_or(workspace_root))?;

    let git_probe = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workspace_root)
        .output();

    if matches!(git_probe, Ok(output) if output.status.success()) {
        let output = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "--quiet",
                "--detach",
                &worktree_path.display().to_string(),
                "HEAD",
            ])
            .current_dir(workspace_root)
            .output()?;
        if !output.status.success() {
            anyhow::bail!(
                "git worktree add failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    } else {
        std::fs::create_dir_all(&worktree_path)?;
    }

    Ok(worktree_path)
}

fn collect_changed_files(
    result: &ToolResult,
    worktree_path: &Path,
    changed_files: &mut BTreeSet<String>,
) {
    let Some(meta) = result.metadata.as_ref() else {
        return;
    };
    if let Some(path) = meta.get("file_path").and_then(Value::as_str) {
        changed_files.insert(path.to_string());
    }
    if let Some(path) = meta.get("resolved_path").and_then(Value::as_str) {
        let resolved = Path::new(path);
        if let Ok(rel) = resolved.strip_prefix(worktree_path) {
            changed_files.insert(rel.to_string_lossy().to_string());
        }
    }
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

    #[tokio::test]
    async fn session_switch_resets_model_when_target_has_no_pinned_model() {
        let dir = tempdir().unwrap();
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

        runtime.set_model("custom/model".to_string());
        assert_eq!(runtime.model_name, "custom/model");

        let mut unpinned = new_session_snapshot(dir.path(), Some("unpinned".to_string()));
        unpinned.metadata.session_id = "unpinned-session".to_string();
        unpinned.metadata.pinned_model = None;
        runtime.store.save_snapshot(&unpinned).unwrap();

        runtime
            .switch_session_by_id("unpinned-session")
            .await
            .unwrap();

        assert_eq!(runtime.model_name, "demo-model");
        assert_eq!(runtime.model_display(), "demo-model");
        assert!(runtime.snapshot().metadata.pinned_model.is_none());
    }

    #[tokio::test]
    async fn background_poll_persists_updates_to_session_snapshot() {
        let dir = tempdir().unwrap();
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
        let session_id = runtime.snapshot().metadata.session_id.clone();

        runtime.subagent_bus.publish_for_session(
            &session_id,
            BackgroundJob {
                id: "job-1".to_string(),
                title: "phase 1".to_string(),
                status: BackgroundJobStatus::Completed,
                detail: "persist me".to_string(),
                kind: BackgroundJobKind::SubAgent,
                progress: Some(100),
                metadata: None,
            },
        );

        let events = runtime.poll_background_events().unwrap();
        assert_eq!(events.len(), 1);

        let loaded = runtime
            .store
            .load_snapshot(&session_id)
            .unwrap()
            .expect("saved snapshot");
        assert_eq!(loaded.metadata.background_jobs, 1);
        assert_eq!(loaded.background_jobs[0].id, "job-1");
        assert_eq!(loaded.background_jobs[0].detail, "persist me");
    }

    #[tokio::test]
    async fn background_updates_do_not_leak_across_session_switches() {
        let dir = tempdir().unwrap();
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
        let first_session = runtime.snapshot().metadata.session_id.clone();
        let mut second = new_session_snapshot(dir.path(), Some("second".to_string()));
        second.metadata.session_id = "second-session".to_string();
        runtime.store.save_snapshot(&second).unwrap();

        runtime
            .switch_session_by_id("second-session")
            .await
            .unwrap();

        runtime.subagent_bus.publish_for_session(
            &first_session,
            BackgroundJob {
                id: "job-a".to_string(),
                title: "old session job".to_string(),
                status: BackgroundJobStatus::Completed,
                detail: "belongs to the first session".to_string(),
                kind: BackgroundJobKind::SubAgent,
                progress: Some(100),
                metadata: None,
            },
        );

        let second_events = runtime.poll_background_events().unwrap();
        assert!(second_events.is_empty());
        assert!(runtime.snapshot().background_jobs.is_empty());

        runtime.switch_session_by_id(&first_session).await.unwrap();
        let first_events = runtime.poll_background_events().unwrap();
        assert_eq!(first_events.len(), 1);
        assert_eq!(runtime.snapshot().background_jobs[0].id, "job-a");
    }

    #[tokio::test]
    async fn spawned_subagent_runs_model_and_persists_summary() {
        let dir = tempdir().unwrap();
        let model = fake_model(vec![Message {
            role: "assistant".to_string(),
            content: Some("sub-agent found the auth boundary".to_string()),
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
        let session_id = runtime.snapshot().metadata.session_id.clone();

        runtime.spawn_subagent("audit auth boundary".to_string());

        let mut completed = None;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            let _ = runtime.poll_background_events().unwrap();
            completed = runtime
                .snapshot()
                .background_jobs
                .iter()
                .find(|job| job.status == BackgroundJobStatus::Completed)
                .cloned();
            if completed.is_some() {
                break;
            }
        }

        let completed = completed.expect("sub-agent should complete from model response");
        assert!(
            completed
                .detail
                .contains("sub-agent found the auth boundary")
        );

        let loaded = runtime
            .store
            .load_snapshot(&session_id)
            .unwrap()
            .expect("saved snapshot");
        assert!(
            loaded
                .background_jobs
                .iter()
                .any(|job| job.detail.contains("sub-agent found the auth boundary"))
        );
    }

    #[tokio::test]
    async fn spawned_subagent_writes_inside_git_worktree_not_root() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let model = fake_model(vec![
            Message {
                role: "assistant".to_string(),
                content: Some("writing in isolated workspace".to_string()),
                tool_calls: Some(vec![ToolCallBlock {
                    id: "call-write".to_string(),
                    r#type: "function".to_string(),
                    function: FunctionCall {
                        name: "write_file".to_string(),
                        arguments: serde_json::json!({
                            "file_path": "subagent-output.txt",
                            "content": "from isolated sub-agent"
                        })
                        .to_string(),
                    },
                }]),
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            },
            Message {
                role: "assistant".to_string(),
                content: Some("isolated write complete".to_string()),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            },
        ]);

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

        runtime.spawn_subagent("write isolated output".to_string());

        let mut completed = None;
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            let _ = runtime.poll_background_events().unwrap();
            completed = runtime
                .snapshot()
                .background_jobs
                .iter()
                .find(|job| job.status == BackgroundJobStatus::Completed)
                .cloned();
            if completed.is_some() {
                break;
            }
        }

        let completed = completed.expect("sub-agent should complete");
        assert!(
            !dir.path().join("subagent-output.txt").exists(),
            "sub-agent writes must not touch the primary workspace"
        );

        let metadata = completed.metadata.expect("sub-agent metadata");
        let worktree_path = metadata
            .get("worktree_path")
            .and_then(|value| value.as_str())
            .expect("worktree_path");
        assert_ne!(std::path::Path::new(worktree_path), dir.path());
        assert_eq!(
            std::fs::read_to_string(
                std::path::Path::new(worktree_path).join("subagent-output.txt")
            )
            .unwrap(),
            "from isolated sub-agent"
        );
        assert!(
            metadata
                .get("changed_files")
                .and_then(|value| value.as_array())
                .unwrap()
                .iter()
                .any(|value| value.as_str() == Some("subagent-output.txt"))
        );
    }

    fn init_git_repo(path: &Path) {
        fn git(path: &Path, args: &[&str]) {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?} failed: {}{}",
                args,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        git(path, &["init", "-q"]);
        git(path, &["config", "user.email", "test@example.com"]);
        git(path, &["config", "user.name", "Test User"]);
        std::fs::write(path.join("README.md"), "fixture\n").unwrap();
        git(path, &["add", "README.md"]);
        git(path, &["commit", "-q", "-m", "init"]);
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
