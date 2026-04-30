use super::mcp::{call_mcp_tool, discover_mcp_tools, refresh_mcp_snapshot};
use super::router::{decide_intent, parse_slash_override, requires_tool_approval, tool_risk};
use super::subagent::{SubAgentBus, SubAgentReport, spawn_executor_subagent};
use super::types::{
    ApprovalRequest, ApprovalStatus, AutonomyLevel, BackgroundJob, BackgroundJobKind,
    BackgroundJobStatus, LspSnapshot, McpSnapshot, RouterIntent, RuntimeEvent, SessionLifecycle,
    ToolExecution, VerificationState, WorkspacePreflight,
};
use super::workspace::{build_preflight, collect_lsp_snapshot, refresh_lsp_snapshot};
use crate::agent::context_compressor::ContextCompressor;
use crate::agent::parser::{ParsedToolCall, ToolParser};
use crate::agent::prompt::{PromptAssembler, SessionPromptContext};
use crate::agent::reference_broker::{
    FindingKind, PackageId, RawFinding, ReferenceBroker, ReferenceConfidence, ReferencePack,
    ReferenceSourceKind,
};
use crate::agent::task_concretizer::{TaskConcretizer, TaskContract};
use crate::agent::token_saver::{
    MinifyRequest, PreservePolicy, SourceKind, TokenBudget, TokenSaver,
};
use crate::cli::InteractiveRequest;
use crate::core::{RiskClass, ToolCall, ToolResult, WorkspaceState, resolve_workspace_path};
use crate::harness::PlanManager;
use crate::harness::session::{
    SessionMetadata, SessionSelection, SessionSnapshot, SessionStatus, SessionStore,
    TranscriptEntry,
};
use crate::harness::trace::{AgentTraceStore, TraceEntry};
use crate::providers::client::ProviderClient;
use crate::providers::factory::{Provider, resolve_model_selection, resolve_provider_auth};
use crate::providers::sse::{StreamChunk, accumulate_stream_to_response};
use crate::providers::types::{ChatRequest, Message, ToolSchema, Usage};
use crate::retrieval::worker::RetrievalWorker;
use crate::tools::{FastExecutor, ToolRegistry};
use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};
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

struct PreparedToolCall {
    id: String,
    call: ToolCall,
    tool_name: String,
    risk: RiskClass,
    execution: ToolExecution,
}

impl PreparedToolCall {
    fn from_parsed(parsed: ParsedToolCall) -> Self {
        let tool_name = tool_name(&parsed.call).to_string();
        Self {
            id: parsed.id,
            risk: tool_risk(&parsed.call),
            execution: ToolExecution {
                tool_name: tool_name.clone(),
                summary: serde_json::to_string(&parsed.call).unwrap_or_else(|_| tool_name.clone()),
                result_preview: None,
            },
            tool_name,
            call: parsed.call,
        }
    }
}

enum ToolCallFlow {
    Continue,
    AwaitingApproval,
}

#[derive(Clone, Copy)]
enum EvidenceBrowserView {
    All,
    Repo,
    References,
}

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
    reference_broker: ReferenceBroker,
    trace_store: AgentTraceStore,
    token_saver: TokenSaver,
    current_turn_id: Option<String>,
    turn_repo_evidence_seen: bool,
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
        let session_id = snapshot.metadata.session_id.clone();

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
            reference_broker: ReferenceBroker::new(),
            trace_store: AgentTraceStore::new(workspace_root, session_id),
            token_saver: TokenSaver::new(),
            current_turn_id: None,
            turn_repo_evidence_seen: false,
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

        self.prepare_turn_harness(message).await?;
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
            let _ = event_tx.send(RuntimeEvent::StreamDone { model: None });
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
            let _ = event_tx.send(RuntimeEvent::StreamDone { model: None });
            self.save()?;
            return Ok(());
        }

        self.prepare_turn_harness(message).await?;
        self.append_transcript("user", message.to_string());
        self.snapshot.messages.push(Message {
            role: "user".to_string(),
            content: Some(message.to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        });

        if let Err(err) = self.run_model_loop_streaming(&event_tx).await {
            let _ = event_tx.send(RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!("Turn failed: {err}"),
            });
            let _ = event_tx.send(RuntimeEvent::StreamDone { model: None });
            self.save()?;
            return Ok(());
        }
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

        let result = self
            .execute_tool_with_gates(&tool_call, &approval.tool_name)
            .await;
        self.record_tool_result(&tool_call, &approval.tool_name, &result)
            .await?;
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
        let system = self
            .prompt_assembler
            .assemble_session_system(SessionPromptContext {
                workspace: &self.workspace_state,
                intent: self.snapshot.metadata.router_intent,
                autonomy: self.autonomy,
                preflight: &self.preflight,
                lsp: &self.lsp,
                mcp: &self.mcp,
                task_contract: self.snapshot.current_task_contract.as_ref(),
                verification: &self.snapshot.verification,
                repo_evidence: &self.snapshot.repo_evidence,
                reference_packs: &self.snapshot.reference_packs,
            });
        self.snapshot.messages[0] = Message {
            role: "system".to_string(),
            content: Some(system),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        };
    }

    async fn prepare_turn_harness(&mut self, message: &str) -> anyhow::Result<()> {
        let turn_id = Uuid::new_v4().to_string();
        self.current_turn_id = Some(turn_id.clone());
        self.turn_repo_evidence_seen = false;
        let contract = TaskConcretizer::new().concretize_for_auto(message);
        self.snapshot.verification = verification_from_contract(&contract);
        self.snapshot.current_task_contract = Some(contract.clone());
        self.snapshot.repo_evidence = self.collect_repo_evidence(message).await?;
        self.snapshot.reference_packs = self.collect_reference_packs(message).await;
        if !self.snapshot.repo_evidence.is_empty() {
            self.turn_repo_evidence_seen = true;
            self.trace(
                Some(&turn_id),
                "repo_evidence",
                serde_json::json!({
                    "query": message,
                    "evidence": self.snapshot.repo_evidence,
                }),
            )?;
        }
        if !self.snapshot.reference_packs.is_empty() {
            self.trace(
                Some(&turn_id),
                "reference_gate",
                serde_json::json!({
                    "query": message,
                    "reference_packs": self.snapshot.reference_packs,
                }),
            )?;
        }
        self.trace(
            Some(&turn_id),
            "task_contract",
            serde_json::json!({
                "contract": contract,
                "verification": self.snapshot.verification,
            }),
        )?;
        self.refresh_system_prompt();
        Ok(())
    }

    async fn collect_repo_evidence(
        &self,
        message: &str,
    ) -> anyhow::Result<Vec<crate::retrieval::types::Evidence>> {
        let worker = RetrievalWorker::new(&self.workspace_root);
        let mut evidence = Vec::new();
        for query in evidence_queries_for_message(message) {
            let result = worker.retrieve(&query, 8).await?;
            evidence.extend(result.evidence);
            if evidence.len() >= 8 {
                break;
            }
        }
        evidence.sort_by(|a, b| {
            b.rank
                .partial_cmp(&a.rank)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        evidence.dedup_by(|a, b| a.file_path == b.file_path && a.line == b.line);
        evidence.truncate(8);
        Ok(evidence)
    }

    async fn collect_reference_packs(&mut self, message: &str) -> Vec<ReferencePack> {
        if !reference_gate_required(message) {
            return Vec::new();
        }

        let packages = self.reference_broker.resolve_packages(&self.workspace_root);
        let mentioned = reference_packages_for_message(message, packages);
        let roots = local_reference_source_roots(&self.workspace_root);
        let mut packs = Vec::new();
        for package in mentioned {
            match self
                .reference_broker
                .fetch_from_local_source_roots(&package, &roots, message)
            {
                Ok(pack) => packs.push(pack),
                Err(_) => match self.reference_broker.fetch_docs(&package).await {
                    Ok(pack) => packs.push(pack),
                    Err(_) => packs.push(local_reference_gate_pack(
                        &self.reference_broker,
                        message,
                        package,
                    )),
                },
            }
        }
        packs
    }

    fn trace(
        &self,
        turn_id: Option<&str>,
        event: &str,
        payload: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.trace_store.append(turn_id, event, payload)
    }

    fn trace_current_turn(&self, event: &str, payload: serde_json::Value) -> anyhow::Result<()> {
        self.trace(self.current_turn_id.as_deref(), event, payload)
    }

    async fn record_tool_result(
        &mut self,
        call: &ToolCall,
        tool_name: &str,
        result: &ToolResult,
    ) -> anyhow::Result<()> {
        self.observe_verification(call, result);
        self.maybe_force_external_precedent(call, result).await;
        let minified = self.token_saver.minify(MinifyRequest {
            source_kind: source_kind_for_tool(call),
            raw: result.output.clone(),
            budget: TokenBudget::new(1000),
            preserve: PreservePolicy::default(),
        });
        self.trace_current_turn(
            "tool_result",
            serde_json::json!({
                "tool_name": tool_name,
                "call": call,
                "success": result.success,
                "output": result.output,
                "minified_output": minified,
                "error": result.error,
                "metadata": result.metadata,
                "verification": self.snapshot.verification,
            }),
        )
    }

    async fn execute_tool_with_gates(&mut self, call: &ToolCall, tool_name: &str) -> ToolResult {
        if let Some(result) = self.scope_guard_result(call, tool_name) {
            return result;
        }
        if requires_repo_evidence_before_execution(call) && !self.turn_repo_evidence_seen {
            let result = ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Tool policy gate: inspect relevant repository context before editing files."
                        .to_string(),
                ),
                metadata: Some(serde_json::json!({
                    "blocked_by": "repo_evidence_gate",
                    "tool_name": tool_name,
                })),
            };
            let _ = self.trace_current_turn(
                "tool_policy_blocked",
                serde_json::json!({
                    "tool_name": tool_name,
                    "call": call,
                    "reason": "repo evidence required before file edits",
                }),
            );
            return result;
        }

        let result = execute_tool_graceful(&mut self.registry, call).await;
        if tool_provides_repo_evidence(call) && result.success {
            self.turn_repo_evidence_seen = true;
        }
        result
    }

    async fn process_prepared_tool_calls<F>(
        &mut self,
        prepared: Vec<PreparedToolCall>,
        mut emit: F,
    ) -> anyhow::Result<ToolCallFlow>
    where
        F: FnMut(RuntimeEvent),
    {
        if prepared.is_empty() {
            return Ok(ToolCallFlow::Continue);
        }

        let mut index = 0usize;
        let mut mutating_barrier: Option<String> = None;
        while index < prepared.len() {
            let batch_len = self.parallel_tool_batch_len(&prepared[index..]);
            if batch_len > 1 {
                let batch = &prepared[index..index + batch_len];
                for item in batch {
                    emit(RuntimeEvent::ToolCallStarted {
                        execution: item.execution.clone(),
                    });
                }

                let results = self.execute_parallel_tool_batch(batch).await?;
                for (item, result) in batch.iter().zip(results.into_iter()) {
                    self.store_tool_result_message(
                        &item.call,
                        &item.tool_name,
                        item.id.clone(),
                        &result,
                    )
                    .await?;
                    emit(RuntimeEvent::ToolCallFinished {
                        execution: item.execution.clone(),
                        result,
                    });
                }
                index += batch_len;
                continue;
            }

            let item = &prepared[index];
            if let Some(reason) = mutating_barrier
                .as_deref()
                .filter(|_| tool_is_ordered_mutation(&item.call))
            {
                emit(RuntimeEvent::ToolCallStarted {
                    execution: item.execution.clone(),
                });
                let result = self.mutating_scheduler_block_result(item, reason);
                self.store_tool_result_message(
                    &item.call,
                    &item.tool_name,
                    item.id.clone(),
                    &result,
                )
                .await?;
                emit(RuntimeEvent::ToolCallFinished {
                    execution: item.execution.clone(),
                    result,
                });
                index += 1;
                continue;
            }

            if let Some(result) = self.scope_guard_result(&item.call, &item.tool_name) {
                emit(RuntimeEvent::ToolCallStarted {
                    execution: item.execution.clone(),
                });
                self.store_tool_result_message(
                    &item.call,
                    &item.tool_name,
                    item.id.clone(),
                    &result,
                )
                .await?;
                if let Some(reason) = mutation_barrier_reason(&item.call, &item.tool_name, &result)
                {
                    mutating_barrier = Some(reason);
                }
                emit(RuntimeEvent::ToolCallFinished {
                    execution: item.execution.clone(),
                    result,
                });
                index += 1;
                continue;
            }

            if requires_tool_approval(self.autonomy, &item.call) {
                let approval = ApprovalRequest {
                    id: Uuid::new_v4().to_string(),
                    tool_name: item.tool_name.clone(),
                    summary: item.execution.summary.clone(),
                    risk: item.risk.clone(),
                    status: ApprovalStatus::Pending,
                    created_at: Utc::now(),
                    tool_arguments: Some(serialize_tool_call(&item.call)?),
                    tool_call_id: Some(item.id.clone()),
                };
                self.snapshot.approvals.push(approval.clone());
                self.refresh_counts();
                emit(RuntimeEvent::ApprovalRequested { approval });
                return Ok(ToolCallFlow::AwaitingApproval);
            }

            if let Some(warn) = self.yolo_bypass_event(&item.tool_name, &item.risk) {
                emit(warn);
            }

            emit(RuntimeEvent::ToolCallStarted {
                execution: item.execution.clone(),
            });
            let result = self
                .execute_tool_with_gates(&item.call, &item.tool_name)
                .await;
            if let Some(event) = self.running_command_event(&result) {
                emit(event);
            }
            self.store_tool_result_message(&item.call, &item.tool_name, item.id.clone(), &result)
                .await?;
            if let Some(reason) = mutation_barrier_reason(&item.call, &item.tool_name, &result) {
                mutating_barrier = Some(reason);
            }
            emit(RuntimeEvent::ToolCallFinished {
                execution: item.execution.clone(),
                result,
            });
            index += 1;
        }

        Ok(ToolCallFlow::Continue)
    }

    fn mutating_scheduler_block_result(
        &self,
        item: &PreparedToolCall,
        prior_barrier: &str,
    ) -> ToolResult {
        let result = ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!(
                "Tool scheduler: skipped mutating tool after prior mutation barrier: {prior_barrier}"
            )),
            metadata: Some(serde_json::json!({
                "blocked_by": "mutating_scheduler",
                "tool_name": item.tool_name,
                "prior_barrier": prior_barrier,
            })),
        };
        let _ = self.trace_current_turn(
            "tool_policy_blocked",
            serde_json::json!({
                "tool_name": item.tool_name,
                "call": item.call,
                "reason": "mutating scheduler blocked later mutation after prior barrier",
                "prior_barrier": prior_barrier,
                "result": result,
            }),
        );
        result
    }

    fn parallel_tool_batch_len(&self, prepared: &[PreparedToolCall]) -> usize {
        let len = prepared
            .iter()
            .take_while(|item| self.can_parallelize_tool_item(item))
            .count();
        if len > 1 { len } else { 0 }
    }

    fn can_parallelize_tool_item(&self, item: &PreparedToolCall) -> bool {
        tool_can_run_in_parallel_batch(&item.call)
            && matches!(item.risk, RiskClass::SafeRead)
            && !requires_tool_approval(self.autonomy, &item.call)
            && self
                .scope_guard_result(&item.call, &item.tool_name)
                .is_none()
    }

    async fn execute_parallel_tool_batch(
        &mut self,
        prepared: &[PreparedToolCall],
    ) -> anyhow::Result<Vec<ToolResult>> {
        self.trace_current_turn(
            "parallel_tool_batch",
            serde_json::json!({
                "tool_count": prepared.len(),
                "tools": prepared.iter().map(|item| item.tool_name.as_str()).collect::<Vec<_>>(),
                "tool_call_ids": prepared.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
            }),
        )?;

        let calls = prepared
            .iter()
            .map(|item| item.call.clone())
            .collect::<Vec<_>>();
        let mut results = FastExecutor::execute_batch(calls, &mut self.registry).await?;
        results.resize_with(prepared.len(), || ToolResult {
            success: false,
            output: String::new(),
            error: Some("Tool execution failed: missing parallel batch result".to_string()),
            metadata: None,
        });
        results.truncate(prepared.len());

        for (item, result) in prepared.iter().zip(results.iter()) {
            if tool_provides_repo_evidence(&item.call) && result.success {
                self.turn_repo_evidence_seen = true;
            }
        }

        Ok(results)
    }

    fn running_command_event(&mut self, result: &ToolResult) -> Option<RuntimeEvent> {
        if !result
            .metadata
            .as_ref()
            .and_then(|meta| meta.get("running"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return None;
        }

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
        Some(RuntimeEvent::BackgroundJobUpdated { job })
    }

    fn scope_guard_result(&self, call: &ToolCall, tool_name: &str) -> Option<ToolResult> {
        let contract = self.snapshot.current_task_contract.as_ref()?;
        let targets = tool_scope_targets(call);
        if targets.is_empty() {
            return None;
        }
        let allowed_scope = self.scope_guard_allowed_patterns(contract);
        if allowed_scope.is_empty() {
            return None;
        }

        for target in targets {
            let normalized_target = match self.workspace_relative_target(&target) {
                Ok(target) => target,
                Err(err) => {
                    return Some(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Tool policy gate: {err}")),
                        metadata: Some(serde_json::json!({
                            "blocked_by": "scope_guard",
                            "tool_name": tool_name,
                            "target": target,
                            "allowed_scope": allowed_scope,
                        })),
                    });
                }
            };
            if scope_allows_target(&normalized_target, &allowed_scope) {
                continue;
            }

            let result = ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Tool policy gate: target '{normalized_target}' is outside current task scope ({})",
                    allowed_scope.join(", ")
                )),
                metadata: Some(serde_json::json!({
                    "blocked_by": "scope_guard",
                    "tool_name": tool_name,
                    "target": normalized_target,
                    "allowed_scope": allowed_scope,
                })),
            };
            let _ = self.trace_current_turn(
                "tool_policy_blocked",
                serde_json::json!({
                    "tool_name": tool_name,
                    "call": call,
                    "reason": "tool target outside current task scope",
                    "result": result,
                }),
            );
            return Some(result);
        }
        None
    }

    fn workspace_relative_target(&self, target: &str) -> Result<String, String> {
        let resolved = resolve_workspace_path(target, &self.workspace_root)?;
        let root = self.workspace_root.canonicalize().map_err(|err| {
            format!(
                "cannot canonicalize workspace root '{}': {err}",
                self.workspace_root.display()
            )
        })?;
        let relative = resolved.strip_prefix(&root).map_err(|_| {
            format!(
                "target '{}' resolves outside workspace root '{}'",
                target,
                root.display()
            )
        })?;
        Ok(path_to_slash(relative))
    }

    fn scope_guard_allowed_patterns(&self, contract: &TaskContract) -> Vec<String> {
        let mut patterns = concrete_scope_patterns(contract);
        for anchor in &contract.repo_anchors {
            if let Some(path) = anchor
                .file_path
                .as_deref()
                .and_then(normalize_scope_pattern)
            {
                push_unique_string(&mut patterns, path);
            }
        }
        for evidence in &self.snapshot.repo_evidence {
            if let Some(path) = normalize_scope_pattern(&evidence.file_path) {
                push_unique_string(&mut patterns, path);
            }
        }
        patterns
    }

    async fn store_tool_result_message(
        &mut self,
        call: &ToolCall,
        tool_name: &str,
        tool_call_id: String,
        result: &ToolResult,
    ) -> anyhow::Result<()> {
        self.record_tool_result(call, tool_name, result).await?;
        self.snapshot.messages.push(Message {
            role: "tool".to_string(),
            content: Some(serde_json::to_string(result)?),
            tool_calls: None,
            tool_call_id: Some(tool_call_id),
            reasoning: None,
            reasoning_details: None,
        });
        self.append_transcript("tool", transcript_preview(tool_name, result));
        Ok(())
    }

    fn observe_verification(&mut self, call: &ToolCall, result: &ToolResult) {
        let Some(command) = verification_command(call, result) else {
            return;
        };
        let status = if result.success { "passed" } else { "failed" };
        self.snapshot
            .verification
            .observed
            .push(format!("command {status}: {command}"));
        self.snapshot.verification.satisfied = result.success
            && !result
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("running"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
        self.snapshot.verification.last_status = Some(format!("command {status}: {command}"));
        self.snapshot.verification.updated_at = Some(Utc::now());
    }

    async fn maybe_force_external_precedent(&mut self, call: &ToolCall, result: &ToolResult) {
        if result.success {
            return;
        }
        if result
            .metadata
            .as_ref()
            .and_then(|meta| meta.get("running"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return;
        }
        let Some(command) = verification_command(call, result) else {
            return;
        };
        let failed_count = consecutive_failed_command_count(&self.snapshot.verification.observed);
        if failed_count < 2 {
            return;
        }

        let signature = failure_signature(result);
        let query = format!("{} {}", command, signature);
        if self.snapshot.reference_packs.iter().any(|pack| {
            pack.source_kind == ReferenceSourceKind::GitHubIssues && pack.query == query
        }) {
            return;
        }

        let issue_findings = self
            .fetch_external_precedent_findings(&command, &signature)
            .await;
        let findings = if issue_findings.is_empty() {
            vec![RawFinding {
                kind: FindingKind::Caveat,
                title: Some("External precedent required".to_string()),
                content: format!(
                    "Local debugging has failed {failed_count} times. Stop guessing and search official issues, changelogs, migration guides, or known fixes before applying another local fix. Last failure: {signature}"
                ),
                language: None,
                source_url: None,
            }]
        } else {
            issue_findings
        };
        let mut pack = self.reference_broker.compile_reference_pack(
            ReferenceSourceKind::GitHubIssues,
            findings,
            &query,
        );
        pack.confidence = if pack.source_refs.is_empty() {
            ReferenceConfidence::Uncertain
        } else {
            ReferenceConfidence::Medium
        };
        pack.relevant_rules.push(
            "After two failed local fix cycles, stop guessing and verify external precedent before the next implementation attempt."
                .to_string(),
        );
        self.snapshot.reference_packs.push(pack.clone());
        self.snapshot.verification.last_status = Some(format!(
            "external precedent required after {failed_count} failed command attempts: {command}"
        ));
        self.snapshot.verification.updated_at = Some(Utc::now());
        self.append_transcript(
            "system",
            format!(
                "External precedent required after {failed_count} failed command attempts. Search known fixes before continuing: {signature}"
            ),
        );
        let _ = self.trace_current_turn(
            "external_precedent_required",
            serde_json::json!({
                "command": command,
                "failed_count": failed_count,
                "signature": signature,
                "reference_pack": pack,
            }),
        );
    }

    async fn fetch_external_precedent_findings(
        &self,
        command: &str,
        signature: &str,
    ) -> Vec<RawFinding> {
        let packages = self.reference_broker.resolve_packages(&self.workspace_root);
        let package = packages
            .into_iter()
            .find(|package| {
                let name = package.name.to_ascii_lowercase();
                command.to_ascii_lowercase().contains(&name)
                    || signature.to_ascii_lowercase().contains(&name)
            })
            .unwrap_or_else(|| PackageId {
                name: workspace_package_name(&self.workspace_root),
                version: None,
                registry: None,
            });

        let mut findings = Vec::new();
        if let Ok(issues) = self
            .reference_broker
            .search_issues(&package, signature)
            .await
        {
            findings.extend(issues.into_iter().map(|issue| RawFinding {
                kind: FindingKind::Caveat,
                title: Some(format!("GitHub precedent: {}", issue.title)),
                content: format!(
                    "{:?} issue matched repeated local failure with relevance {:.2}: {}",
                    issue.status, issue.relevance_score, issue.title
                ),
                language: None,
                source_url: Some(issue.url),
            }));
        }
        if let Ok(discussions) = self
            .reference_broker
            .search_discussions(&package, signature)
            .await
        {
            findings.extend(discussions.into_iter().map(|discussion| {
                let status = if discussion.answered {
                    "Answered"
                } else {
                    "Open"
                };
                RawFinding {
                    kind: FindingKind::Caveat,
                    title: Some(format!("GitHub discussion precedent: {}", discussion.title)),
                    content: format!(
                        "{status} discussion matched repeated local failure with relevance {:.2}: {}",
                        discussion.relevance_score, discussion.title
                    ),
                    language: None,
                    source_url: Some(discussion.url),
                }
            }));
        }
        findings
    }

    fn verification_gap_event(&mut self, content: &str) -> Option<RuntimeEvent> {
        if self.snapshot.verification.satisfied
            || !parsed_completion_claim_without_verification(content)
        {
            return None;
        }

        let message = "Verification gate not satisfied: assistant made a completion claim before observed verification evidence. Run or report verification before treating this as complete.".to_string();
        self.snapshot.verification.last_status =
            Some("completion claim blocked: verification evidence missing".to_string());
        self.snapshot.verification.updated_at = Some(Utc::now());
        self.append_transcript("system", message.clone());
        let _ = self.trace_current_turn(
            "verification_gap",
            serde_json::json!({
                "content": content,
                "verification": self.snapshot.verification,
            }),
        );

        Some(RuntimeEvent::MessageDelta {
            role: "system".to_string(),
            content: message,
        })
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

            self.trace_current_turn(
                "model_response",
                serde_json::json!({
                    "role": response.role.clone(),
                    "content": response.content.clone(),
                    "tool_call_count": response.tool_calls.as_ref().map(Vec::len).unwrap_or(0),
                }),
            )?;

            if let Some(content) = response.content.clone()
                && !content.trim().is_empty()
            {
                self.append_transcript("assistant", content.clone());
                events.push(RuntimeEvent::MessageDelta {
                    role: "assistant".to_string(),
                    content: content.clone(),
                });
                let has_tool_calls = response
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| !calls.is_empty());
                if !has_tool_calls {
                    events.extend(self.verification_gap_event(&content));
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

            let prepared = parsed_calls
                .into_iter()
                .map(PreparedToolCall::from_parsed)
                .collect::<Vec<_>>();
            if matches!(
                self.process_prepared_tool_calls(prepared, |event| events.push(event))
                    .await?,
                ToolCallFlow::AwaitingApproval
            ) {
                return Ok(events);
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
                            if let Some(ref content) = choice.delta.content
                                && !content.is_empty()
                            {
                                accumulated_content.push_str(content);
                                let _ = event_tx.send(RuntimeEvent::StreamDelta {
                                    role: "assistant".to_string(),
                                    content: content.clone(),
                                    model: chunk.model.clone(),
                                });
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

            self.trace_current_turn(
                "model_response",
                serde_json::json!({
                    "role": choice.message.role.clone(),
                    "content": choice.message.content.clone(),
                    "tool_call_count": choice.message.tool_calls.as_ref().map(Vec::len).unwrap_or(0),
                    "streamed": true,
                }),
            )?;

            let assistant_content_for_gap = choice.message.content.clone();
            if let Some(ref content) = assistant_content_for_gap
                && !content.trim().is_empty()
            {
                self.append_transcript("assistant", content.clone());
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
                let gap_event = assistant_content_for_gap
                    .as_deref()
                    .and_then(|content| self.verification_gap_event(content));
                if let Some(event) = gap_event {
                    let _ = event_tx.send(event);
                }
                break;
            }

            let prepared = parsed_calls
                .into_iter()
                .map(PreparedToolCall::from_parsed)
                .collect::<Vec<_>>();
            if matches!(
                self.process_prepared_tool_calls(prepared, |event| {
                    let _ = event_tx.send(event);
                })
                .await?,
                ToolCallFlow::AwaitingApproval
            ) {
                return Ok(());
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
                let _ = event_tx.send(RuntimeEvent::StreamDone { model: None });
                return Ok(());
            }
        };

        self.trace_current_turn(
            "model_response",
            serde_json::json!({
                "role": response.role.clone(),
                "content": response.content.clone(),
                "tool_call_count": response.tool_calls.as_ref().map(Vec::len).unwrap_or(0),
                "streamed": false,
                "fallback": true,
            }),
        )?;

        if let Some(content) = response.content.clone()
            && !content.trim().is_empty()
        {
            let _ = event_tx.send(RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: content.clone(),
            });
            self.append_transcript("assistant", content.clone());
            let has_tool_calls = response
                .tool_calls
                .as_ref()
                .is_some_and(|calls| !calls.is_empty());
            let gap_event = (!has_tool_calls)
                .then(|| self.verification_gap_event(&content))
                .flatten();
            if let Some(event) = gap_event {
                let _ = event_tx.send(event);
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
            let prepared = parsed_calls
                .into_iter()
                .map(PreparedToolCall::from_parsed)
                .collect::<Vec<_>>();
            if matches!(
                self.process_prepared_tool_calls(prepared, |event| {
                    let _ = event_tx.send(event);
                })
                .await?,
                ToolCallFlow::AwaitingApproval
            ) {
                let _ = event_tx.send(RuntimeEvent::StreamDone { model: None });
                return Ok(());
            }
        }

        let _ = event_tx.send(RuntimeEvent::StreamDone { model: None });
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
            ["/help"] | ["/?"] | ["/h"] => {
                Ok(Some(vec![modal_event("Help", help_text())]))
            }
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
            ["/mcp"] => Ok(Some(vec![modal_event("MCP", self.render_mcp_summary())])),
            ["/mcp", "refresh"] => Ok(Some(self.handle_mcp_refresh().await?)),
            ["/lsp"] => Ok(Some(vec![modal_event("LSP", self.render_lsp_summary())])),
            ["/lsp", "refresh"] => Ok(Some(self.handle_lsp_refresh().await?)),
            ["/lsp", "diagnostics"] => Ok(Some(vec![modal_event(
                "LSP Diagnostics",
                self.render_lsp_diagnostics(),
            )])),
            ["/lsp", "symbols"] => {
                Ok(Some(vec![modal_event("LSP Symbols", self.render_lsp_symbols())]))
            }
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
            ["/audit"] => Ok(Some(vec![modal_event(
                "Audit",
                self.render_audit_summary(50),
            )])),
            ["/audit", "insights"] => Ok(Some(vec![modal_event(
                "Audit Insights",
                self.render_audit_insights(100),
            )])),
            ["/audit", "insights", limit] => Ok(Some(vec![modal_event(
                "Audit Insights",
                self.render_audit_insights(parse_audit_limit(limit, 100)),
            )])),
            ["/audit", "replay"] => Ok(Some(vec![modal_event(
                "Audit Replay",
                self.render_audit_replay(20),
            )])),
            ["/audit", "replay", limit] => Ok(Some(vec![modal_event(
                "Audit Replay",
                self.render_audit_replay(parse_audit_limit(limit, 20)),
            )])),
            ["/evidence"] | ["/evidence", "all"] => Ok(Some(vec![modal_event(
                "Evidence",
                self.render_evidence_browser(EvidenceBrowserView::All),
            )])),
            ["/evidence", "repo"] => Ok(Some(vec![modal_event(
                "Evidence",
                self.render_evidence_browser(EvidenceBrowserView::Repo),
            )])),
            ["/evidence", "refs"] | ["/evidence", "references"] => Ok(Some(vec![modal_event(
                "Evidence",
                self.render_evidence_browser(EvidenceBrowserView::References),
            )])),
            ["/clear"] => Ok(Some(self.clear_transcript())),
            ["/new"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: "Start a new session with `charm new` or Ctrl+N. The TUI will spin up a fresh session in a moment.".to_string(),
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
            ["/model", target] => Ok(Some(self.set_model_connected(target.to_string()).await)),
            ["/provider"] | ["/providers"] => Ok(Some(vec![modal_event(
                "Providers",
                self.render_provider_summary(),
            )])),
            ["/provider", "connect", provider] | ["/providers", "connect", provider] => {
                Ok(Some(vec![modal_event(
                    provider_modal_title(provider),
                    provider_connection_content(provider),
                )]))
            }
            ["/agent"] | ["/agents"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: self.render_agent_summary(),
            }])),
            ["/agent", "list"] | ["/agents", "list"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: self.render_agent_summary(),
            }])),
            ["/agent", "diff", id] | ["/agents", "diff", id] => {
                Ok(Some(self.diff_subagent_result(id)))
            }
            ["/agent", "export", id] | ["/agents", "export", id] => {
                Ok(Some(self.export_subagent_result(id)?))
            }
            ["/agent", "pr", id] | ["/agents", "pr", id] => {
                Ok(Some(self.draft_subagent_pr(id)?))
            }
            ["/agent", "merge", id] | ["/agents", "merge", id] => {
                Ok(Some(self.merge_subagent_result(id)?))
            }
            ["/agent", "cleanup", id] | ["/agents", "cleanup", id] => {
                Ok(Some(self.cleanup_subagent_worktree(id)?))
            }
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

    fn render_audit_summary(&self, limit: usize) -> String {
        let entries = match self.trace_store.read_recent(limit) {
            Ok(entries) => entries,
            Err(err) => return format!("Audit unavailable: {err}"),
        };
        if entries.is_empty() {
            return "Audit: no trace entries for this session yet.".to_string();
        }

        let mut counts = BTreeMap::new();
        let mut blocked = 0usize;
        let mut failed_tools = 0usize;
        let mut turn_ids = BTreeSet::new();
        for entry in &entries {
            *counts.entry(entry.event.clone()).or_insert(0usize) += 1;
            if entry.event == "tool_policy_blocked" {
                blocked += 1;
            }
            if entry.event == "tool_result"
                && !entry
                    .payload
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
            {
                failed_tools += 1;
            }
            if let Some(turn_id) = entry.turn_id.as_deref() {
                turn_ids.insert(turn_id.to_string());
            }
        }

        let mut lines = vec![
            format!(
                "Audit: {} recent trace entries, {} turns",
                entries.len(),
                turn_ids.len()
            ),
            format!("Trace file: {}", self.trace_store.trace_path().display()),
            format!("Policy blocks: {blocked}"),
            format!("Failed tool results: {failed_tools}"),
            "Events:".to_string(),
        ];
        for (event, count) in counts {
            lines.push(format!("  {event}: {count}"));
        }
        lines.push("Use /audit replay [n] for timeline replay.".to_string());
        lines.join("\n")
    }

    fn render_audit_replay(&self, limit: usize) -> String {
        let entries = match self.trace_store.read_recent(limit) {
            Ok(entries) => entries,
            Err(err) => return format!("Trace Replay unavailable: {err}"),
        };
        if entries.is_empty() {
            return "Trace Replay: no trace entries for this session yet.".to_string();
        }

        let mut lines = vec![format!("Trace Replay: last {} entries", entries.len())];
        for entry in entries {
            lines.push(format_trace_entry(&entry));
        }
        lines.join("\n")
    }

    fn render_audit_insights(&self, limit: usize) -> String {
        let entries = match self.trace_store.read_recent(limit) {
            Ok(entries) => entries,
            Err(err) => return format!("Audit Insights unavailable: {err}"),
        };
        if entries.is_empty() {
            return "Audit Insights: no trace entries for this session yet.".to_string();
        }

        let insights = analyze_trace_insights(&entries);
        let mut lines = vec![format!("Audit Insights: {} trace entries", entries.len())];

        if insights.repeated_failures.is_empty()
            && insights.policy_blocks == 0
            && insights.verification_gaps == 0
            && !insights.missing_reference_risk
        {
            lines.push("No strong repeated-failure or missing-context signals.".to_string());
            return lines.join("\n");
        }

        if !insights.repeated_failures.is_empty() {
            lines.push("Repeated failures:".to_string());
            for failure in &insights.repeated_failures {
                let logs = if failure.log_refs.is_empty() {
                    "logs=none".to_string()
                } else {
                    format!("logs={}", failure.log_refs.join(", "))
                };
                lines.push(format!(
                    "  - {} x{}: {} ({})",
                    failure.tool_name, failure.count, failure.signature, logs
                ));
            }
        }

        if insights.missing_reference_risk {
            lines.push(format!(
                "Missing reference risk: {} failed tool results, {} reference events.",
                insights.failed_tools, insights.reference_events
            ));
        }
        if insights.policy_blocks > 0 {
            lines.push(format!(
                "Missed context: {} policy block(s).",
                insights.policy_blocks
            ));
        }
        if insights.verification_gaps > 0 {
            lines.push(format!(
                "Verification gaps: {} completion claim(s) lacked verification.",
                insights.verification_gaps
            ));
        }

        let candidates = insights.candidates();
        if !candidates.workflows.is_empty() {
            lines.push("Candidate workflows:".to_string());
            for item in candidates.workflows {
                lines.push(format!("  - {item}"));
            }
        }
        if !candidates.rules.is_empty() {
            lines.push("Candidate rules:".to_string());
            for item in candidates.rules {
                lines.push(format!("  - {item}"));
            }
        }
        if !candidates.memories.is_empty() {
            lines.push("Candidate memories:".to_string());
            for item in candidates.memories {
                lines.push(format!("  - {item}"));
            }
        }
        lines.join("\n")
    }

    fn render_provider_summary(&self) -> String {
        let providers = [
            Provider::OpenRouter,
            Provider::OpenAi,
            Provider::OpenAiCodex,
            Provider::Anthropic,
            Provider::Google,
            Provider::Ollama,
        ];
        let mut lines = vec![
            format!("Current model: {}", self.model_display),
            String::new(),
            "Providers:".to_string(),
        ];
        for provider in providers {
            let status = if resolve_provider_auth(&provider).is_ok() {
                "connected"
            } else {
                "not configured"
            };
            lines.push(format!(
                "  - {:<14} {}",
                provider_display_name(provider.id()),
                status
            ));
        }
        lines.push(String::new());
        lines.push("Use /provider connect <provider> to open the REPL connector.".to_string());
        lines.push("Use /model <provider/model-id> to switch provider and model.".to_string());
        lines.join("\n")
    }

    fn render_evidence_browser(&self, view: EvidenceBrowserView) -> String {
        let session_id = self.snapshot.metadata.session_id.clone();
        let (snapshot, source) = match self.store.load_snapshot(&session_id) {
            Ok(Some(snapshot)) => (snapshot, "persisted"),
            _ => (self.snapshot.clone(), "memory"),
        };
        let short_id = &session_id[..session_id.len().min(8)];
        let mut lines = vec![
            format!(
                "Evidence Browser: session {short_id} ({source}; repo={} refs={})",
                snapshot.repo_evidence.len(),
                snapshot.reference_packs.len()
            ),
            format!(
                "Files: {}",
                self.workspace_root
                    .join(".charm")
                    .join("sessions")
                    .join(&session_id)
                    .display()
            ),
        ];

        if matches!(view, EvidenceBrowserView::All | EvidenceBrowserView::Repo) {
            lines.push("Repo evidence:".to_string());
            if snapshot.repo_evidence.is_empty() {
                lines.push("  (none)".to_string());
            } else {
                for evidence in snapshot.repo_evidence.iter().take(8) {
                    let snippet = truncate_for_prompt(&evidence.snippet.replace('\n', " "), 120);
                    lines.push(format!(
                        "  [{:.2}] {}:{} {} ({})",
                        evidence.rank, evidence.file_path, evidence.line, snippet, evidence.source
                    ));
                }
                if snapshot.repo_evidence.len() > 8 {
                    lines.push(format!("  ... {} more", snapshot.repo_evidence.len() - 8));
                }
            }
        }

        if matches!(
            view,
            EvidenceBrowserView::All | EvidenceBrowserView::References
        ) {
            lines.push("Reference packs:".to_string());
            if snapshot.reference_packs.is_empty() {
                lines.push("  (none)".to_string());
            } else {
                for pack in snapshot.reference_packs.iter().take(8) {
                    let label = pack
                        .library
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or(&pack.query);
                    let version = pack
                        .version
                        .as_ref()
                        .map(|version| format!("@{version}"))
                        .unwrap_or_default();
                    lines.push(format!(
                        "  {:?} {}{} confidence={:?} rules={} examples={} refs={}",
                        pack.source_kind,
                        label,
                        version,
                        pack.confidence,
                        pack.relevant_rules.len(),
                        pack.minimal_examples.len(),
                        pack.source_refs.len()
                    ));
                    if let Some(source) = pack.source_refs.first() {
                        let title = source
                            .title
                            .as_deref()
                            .filter(|value| !value.trim().is_empty())
                            .unwrap_or("source");
                        lines.push(format!("    - {title}: {}", source.url));
                    }
                }
                if snapshot.reference_packs.len() > 8 {
                    lines.push(format!("  ... {} more", snapshot.reference_packs.len() - 8));
                }
            }
        }

        if snapshot.repo_evidence.is_empty() && snapshot.reference_packs.is_empty() {
            lines.push("No persisted evidence yet. Run a normal turn that inspects files or resolves references.".to_string());
        }

        lines.join("\n")
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

    pub async fn set_model_connected(&mut self, target: String) -> Vec<RuntimeEvent> {
        let target = target.trim();
        if target.is_empty() {
            return vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: "Usage: /model <provider/model-id>".to_string(),
            }];
        }

        let selection = match resolve_model_selection(None, target) {
            Ok(selection) => selection,
            Err(err) => {
                return vec![modal_event(
                    "Model",
                    format!("Could not resolve model `{target}`.\n\n{err}"),
                )];
            }
        };
        let auth = match resolve_provider_auth(&selection.provider) {
            Ok(auth) => auth,
            Err(err) => {
                return vec![modal_event(
                    provider_modal_title(selection.provider.id()),
                    format!(
                        "{}\n\nAttempted model: {}\n\n{}",
                        provider_connection_content(selection.provider.id()),
                        selection.display_model,
                        err
                    ),
                )];
            }
        };

        let provider_id = selection.provider.id().to_string();
        self.model = Arc::new(selection.provider.create_client(auth));
        self.model_name = selection.request_model.clone();
        self.model_display = selection.display_model.clone();
        self.snapshot.metadata.pinned_model = Some(selection.display_model.clone());
        self.prompt_assembler =
            PromptAssembler::new(&self.workspace_root).with_provider(&provider_id);
        self.refresh_system_prompt();
        self.append_transcript(
            "system",
            format!(
                "Provider connected: {provider_id}; model pinned to {}",
                selection.display_model
            ),
        );

        vec![
            RuntimeEvent::ModelChanged {
                model: selection.request_model,
                display: selection.display_model.clone(),
            },
            RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!(
                    "Provider connected: {provider_id}. Model set to {}.",
                    selection.display_model
                ),
            },
        ]
    }

    pub fn compact_context(&mut self) -> Vec<RuntimeEvent> {
        let before = self.snapshot.messages.len();
        let raw_compacted = ContextCompressor::compaction_raw(&self.snapshot.messages, 12);
        let minified_evidence = if raw_compacted.trim().is_empty() {
            None
        } else {
            Some(self.token_saver.minify(MinifyRequest {
                source_kind: SourceKind::CommandOutput,
                raw: raw_compacted,
                budget: TokenBudget::new(800),
                preserve: PreservePolicy {
                    head_lines: Some(40),
                    tail_lines: Some(10),
                    ..PreservePolicy::default()
                },
            }))
        };
        let removed = ContextCompressor::compact_now(&mut self.snapshot.messages, 12);
        if removed > 0
            && let Some(minified) = minified_evidence.as_ref()
            && let Some(summary) = self
                .snapshot
                .messages
                .get_mut(1)
                .and_then(|message| message.content.as_mut())
        {
            summary.push_str("\nTokenSaver evidence:\n");
            summary.push_str(&minified.text);
        }
        self.refresh_system_prompt();
        let after = self.snapshot.messages.len();
        let net_removed = before.saturating_sub(after);
        let summary = if removed == 0 {
            "Context already compact.".to_string()
        } else {
            format!(
                "Compacted context: {removed} messages rolled into summary ({net_removed} net removed)."
            )
        };
        let _ = self.trace_current_turn(
            "context_compacted",
            serde_json::json!({
                "removed_messages": removed,
                "net_removed_messages": net_removed,
                "remaining_messages": after,
                "minified_evidence": minified_evidence,
            }),
        );
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
        lines.push(
            "Use /agent diff <id>, /agent export <id>, /agent pr <id>, /agent merge <id>, /agent cleanup <id>, or /agent kill <id>."
                .to_string(),
        );
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

    pub fn diff_subagent_result(&self, id_prefix: &str) -> Vec<RuntimeEvent> {
        let Some(index) = self.find_subagent_job(id_prefix) else {
            return vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!("No sub-agent matches '{id_prefix}'."),
            }];
        };
        let job = &self.snapshot.background_jobs[index];
        let Ok((worktree_path, changed_files)) = self.subagent_worktree_details(job) else {
            return vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!(
                    "Sub-agent {} has no reviewable worktree metadata.",
                    short_id(job)
                ),
            }];
        };

        let status = std::process::Command::new("git")
            .args(["status", "--short"])
            .current_dir(&worktree_path)
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "(no git status output)".to_string());
        let diff = std::process::Command::new("git")
            .arg("diff")
            .arg("--")
            .args(&changed_files)
            .current_dir(&worktree_path)
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "(no tracked diff; files may be new/untracked)".to_string());

        vec![RuntimeEvent::MessageDelta {
            role: "system".to_string(),
            content: format!(
                "Sub-agent {}\nWorktree: {}\nChanged files: {}\n\nStatus:\n{}\n\nDiff:\n{}",
                short_id(job),
                worktree_path.display(),
                if changed_files.is_empty() {
                    "(none)".to_string()
                } else {
                    changed_files.join(", ")
                },
                status,
                diff
            ),
        }]
    }

    pub fn export_subagent_result(&mut self, id_prefix: &str) -> anyhow::Result<Vec<RuntimeEvent>> {
        let Some(index) = self.find_subagent_job(id_prefix) else {
            return Ok(vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!("No sub-agent matches '{id_prefix}'."),
            }]);
        };
        let job = self.snapshot.background_jobs[index].clone();
        let (worktree_path, changed_files) = self.subagent_worktree_details(&job)?;

        let status = std::process::Command::new("git")
            .args(["status", "--short"])
            .current_dir(&worktree_path)
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "(no git status output)".to_string());
        let diff = std::process::Command::new("git")
            .arg("diff")
            .arg("--")
            .args(&changed_files)
            .current_dir(&worktree_path)
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "(no tracked diff; files may be new/untracked)".to_string());

        let mut export = String::new();
        export.push_str(&format!("# Sub-agent Export: {}\n\n", job.title));
        export.push_str(&format!("- Job: {}\n", job.id));
        export.push_str(&format!("- Status: {:?}\n", job.status));
        export.push_str(&format!("- Worktree: {}\n", worktree_path.display()));
        export.push_str(&format!("- Detail: {}\n", job.detail));
        export.push_str(&format!(
            "- Changed files: {}\n\n",
            if changed_files.is_empty() {
                "(none)".to_string()
            } else {
                changed_files.join(", ")
            }
        ));
        export.push_str("## Git Status\n\n```text\n");
        export.push_str(&status);
        export.push_str("\n```\n\n## Diff\n\n```diff\n");
        export.push_str(&diff);
        export.push_str("\n```\n\n");

        if !changed_files.is_empty() {
            let canonical_worktree = worktree_path.canonicalize()?;
            export.push_str("## File Snapshots\n\n");
            for rel in &changed_files {
                validate_agent_relative_path(rel)?;
                let path = canonical_worktree.join(rel);
                let Ok(canonical_path) = path.canonicalize() else {
                    export.push_str(&format!("### {rel}\n\n(missing file)\n\n"));
                    continue;
                };
                anyhow::ensure!(
                    canonical_path.starts_with(&canonical_worktree),
                    "sub-agent export source escapes worktree: {}",
                    rel
                );
                if !canonical_path.is_file() {
                    export.push_str(&format!("### {rel}\n\n(not a file)\n\n"));
                    continue;
                }
                let content = std::fs::read_to_string(&canonical_path)
                    .map(|raw| truncate_for_prompt(&raw, 20_000))
                    .unwrap_or_else(|_| "(binary or unreadable file)".to_string());
                export.push_str(&format!("### {rel}\n\n```text\n{content}\n```\n\n"));
            }
        }

        let export_dir = self.workspace_root.join(".charm").join("exports");
        std::fs::create_dir_all(&export_dir)?;
        let export_path =
            export_dir.join(format!("subagent-{}.md", sanitize_export_filename(&job.id)));
        std::fs::write(&export_path, export)?;

        if let Some(metadata) = self.snapshot.background_jobs[index].metadata.as_mut()
            && let Some(obj) = metadata.as_object_mut()
        {
            obj.insert(
                "export_path".to_string(),
                Value::String(export_path.display().to_string()),
            );
            obj.insert(
                "exported_at".to_string(),
                Value::String(Utc::now().to_rfc3339()),
            );
        }
        let job = self.snapshot.background_jobs[index].clone();
        Ok(vec![
            RuntimeEvent::BackgroundJobUpdated { job },
            RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!(
                    "Exported sub-agent {} review artifact to {}.",
                    short_id(&self.snapshot.background_jobs[index]),
                    export_path.display()
                ),
            },
        ])
    }

    pub fn draft_subagent_pr(&mut self, id_prefix: &str) -> anyhow::Result<Vec<RuntimeEvent>> {
        let Some(index) = self.find_subagent_job(id_prefix) else {
            return Ok(vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!("No sub-agent matches '{id_prefix}'."),
            }]);
        };
        let job = self.snapshot.background_jobs[index].clone();
        let (worktree_path, changed_files) = self.subagent_worktree_details(&job)?;
        let export_path = job
            .metadata
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|metadata| metadata.get("export_path"))
            .and_then(Value::as_str)
            .unwrap_or("(not exported yet)");

        let mut draft = String::new();
        draft.push_str(&format!("# Pull Request Draft: {}\n\n", job.title));
        draft.push_str("## Title\n\n");
        draft.push_str(&job.title);
        draft.push_str("\n\n## Summary\n\n");
        draft.push_str(&format!("- {}\n", job.detail));
        draft.push_str(&format!("- Sub-agent job: {}\n", job.id));
        draft.push_str(&format!("- Source worktree: {}\n", worktree_path.display()));
        draft.push_str(&format!("- Review artifact: {}\n", export_path));
        draft.push_str("\n## Changed Files\n\n");
        if changed_files.is_empty() {
            draft.push_str("- (none)\n");
        } else {
            for rel in &changed_files {
                draft.push_str(&format!("- `{rel}`\n"));
            }
        }
        draft.push_str("\n## Test Plan\n\n");
        draft.push_str("- Review the sub-agent export artifact before merging.\n");
        draft.push_str("- Run the project verification commands after applying the changes.\n");
        draft.push_str("\n## Notes\n\n");
        draft.push_str(
            "Generated locally by Charm. Create the remote PR only after reviewing and merging the isolated worktree changes.\n",
        );

        let export_dir = self.workspace_root.join(".charm").join("exports");
        std::fs::create_dir_all(&export_dir)?;
        let draft_path = export_dir.join(format!(
            "subagent-{}-pr.md",
            sanitize_export_filename(&job.id)
        ));
        std::fs::write(&draft_path, draft)?;

        if let Some(metadata) = self.snapshot.background_jobs[index].metadata.as_mut()
            && let Some(obj) = metadata.as_object_mut()
        {
            obj.insert(
                "pr_draft_path".to_string(),
                Value::String(draft_path.display().to_string()),
            );
            obj.insert(
                "pr_drafted_at".to_string(),
                Value::String(Utc::now().to_rfc3339()),
            );
        }
        let job = self.snapshot.background_jobs[index].clone();
        Ok(vec![
            RuntimeEvent::BackgroundJobUpdated { job },
            RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!(
                    "Wrote sub-agent PR draft for {} to {}.",
                    short_id(&self.snapshot.background_jobs[index]),
                    draft_path.display()
                ),
            },
        ])
    }

    pub fn merge_subagent_result(&mut self, id_prefix: &str) -> anyhow::Result<Vec<RuntimeEvent>> {
        let Some(index) = self.find_subagent_job(id_prefix) else {
            return Ok(vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!("No sub-agent matches '{id_prefix}'."),
            }]);
        };
        let (worktree_path, changed_files) =
            self.subagent_worktree_details(&self.snapshot.background_jobs[index])?;
        if changed_files.is_empty() {
            return Ok(vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!(
                    "Sub-agent {} has no changed files to merge.",
                    short_id(&self.snapshot.background_jobs[index])
                ),
            }]);
        }

        let mut merged = Vec::new();
        let canonical_worktree = worktree_path.canonicalize()?;
        for rel in changed_files {
            validate_agent_relative_path(&rel)?;
            let source = canonical_worktree.join(&rel);
            let canonical_source = source
                .canonicalize()
                .with_context(|| format!("missing sub-agent output file '{}'", source.display()))?;
            anyhow::ensure!(
                canonical_source.starts_with(&canonical_worktree),
                "sub-agent source escapes worktree: {}",
                rel
            );
            anyhow::ensure!(
                canonical_source.is_file(),
                "sub-agent merge source is not a file: {}",
                rel
            );
            let target = resolve_workspace_path(&rel, &self.workspace_root)
                .map_err(|e| anyhow::anyhow!("invalid merge target '{}': {}", rel, e))?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&canonical_source, &target)?;
            merged.push(rel);
        }

        let merged_count = merged.len();
        if let Some(metadata) = self.snapshot.background_jobs[index].metadata.as_mut()
            && let Some(obj) = metadata.as_object_mut()
        {
            obj.insert("merged".to_string(), Value::Bool(true));
            obj.insert(
                "merged_files".to_string(),
                serde_json::json!(merged.clone()),
            );
        }
        let job = self.snapshot.background_jobs[index].clone();
        Ok(vec![
            RuntimeEvent::BackgroundJobUpdated { job },
            RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!(
                    "Merged {} file(s) from sub-agent {}.",
                    merged_count,
                    short_id(&self.snapshot.background_jobs[index])
                ),
            },
        ])
    }

    pub fn cleanup_subagent_worktree(
        &mut self,
        id_prefix: &str,
    ) -> anyhow::Result<Vec<RuntimeEvent>> {
        let Some(index) = self.find_subagent_job(id_prefix) else {
            return Ok(vec![RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!("No sub-agent matches '{id_prefix}'."),
            }]);
        };
        let (worktree_path, _) =
            self.subagent_worktree_details(&self.snapshot.background_jobs[index])?;
        if worktree_path.exists() {
            let _ = std::process::Command::new("git")
                .args([
                    "worktree",
                    "remove",
                    "--force",
                    &worktree_path.display().to_string(),
                ])
                .current_dir(&self.workspace_root)
                .output();
        }
        if worktree_path.exists() {
            std::fs::remove_dir_all(&worktree_path)?;
        }
        if let Some(metadata) = self.snapshot.background_jobs[index].metadata.as_mut()
            && let Some(obj) = metadata.as_object_mut()
        {
            obj.insert("cleaned_up".to_string(), Value::Bool(true));
        }
        let job = self.snapshot.background_jobs[index].clone();
        Ok(vec![
            RuntimeEvent::BackgroundJobUpdated { job },
            RuntimeEvent::MessageDelta {
                role: "system".to_string(),
                content: format!(
                    "Cleaned up sub-agent {} worktree.",
                    short_id(&self.snapshot.background_jobs[index])
                ),
            },
        ])
    }

    fn find_subagent_job(&self, id_prefix: &str) -> Option<usize> {
        self.snapshot.background_jobs.iter().position(|job| {
            matches!(job.kind, BackgroundJobKind::SubAgent) && job.id.starts_with(id_prefix)
        })
    }

    fn subagent_worktree_details(
        &self,
        job: &BackgroundJob,
    ) -> anyhow::Result<(PathBuf, Vec<String>)> {
        let metadata = job
            .metadata
            .as_ref()
            .and_then(Value::as_object)
            .context("missing sub-agent metadata")?;
        let raw_worktree = metadata
            .get("worktree_path")
            .and_then(Value::as_str)
            .context("missing worktree_path")?;
        let worktree_path = PathBuf::from(raw_worktree);
        let canonical_base = self
            .workspace_root
            .join(".charm")
            .join("worktrees")
            .canonicalize()
            .context("missing .charm/worktrees directory")?;
        let canonical_worktree = worktree_path
            .canonicalize()
            .with_context(|| format!("missing worktree '{}'", worktree_path.display()))?;
        anyhow::ensure!(
            canonical_worktree.starts_with(&canonical_base),
            "sub-agent worktree path is outside .charm/worktrees"
        );
        let changed_files = metadata
            .get("changed_files")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for rel in &changed_files {
            validate_agent_relative_path(rel)?;
        }
        Ok((canonical_worktree, changed_files))
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
        self.trace_store = self
            .trace_store
            .for_session(self.snapshot.metadata.session_id.clone());
        self.current_turn_id = None;
        self.turn_repo_evidence_seen = false;
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

fn short_id(job: &BackgroundJob) -> String {
    job.id.chars().take(8).collect()
}

fn validate_agent_relative_path(path: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !path.trim().is_empty(),
        "changed file path must not be empty"
    );
    let rel = Path::new(path);
    anyhow::ensure!(
        rel.is_relative(),
        "changed file path must be relative: {}",
        path
    );
    for component in rel.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("changed file path escapes workspace: {}", path);
            }
        }
    }
    Ok(())
}

fn verification_from_contract(contract: &TaskContract) -> VerificationState {
    VerificationState {
        required: contract.verification.clone(),
        observed: Vec::new(),
        satisfied: false,
        last_status: Some("waiting for verification evidence".to_string()),
        updated_at: Some(Utc::now()),
    }
}

fn verification_command(call: &ToolCall, result: &ToolResult) -> Option<String> {
    match call {
        ToolCall::RunCommand { command, .. } => Some(command.clone()),
        ToolCall::PollCommand { .. } => result
            .metadata
            .as_ref()
            .and_then(|meta| meta.get("command"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

fn consecutive_failed_command_count(observed: &[String]) -> usize {
    observed
        .iter()
        .rev()
        .take_while(|entry| entry.starts_with("command failed:"))
        .count()
}

fn failure_signature(result: &ToolResult) -> String {
    let source = result
        .error
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(&result.output);
    source
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed == "---stderr---" {
                None
            } else {
                Some(trimmed)
            }
        })
        .map(|line| truncate_for_prompt(line, 240))
        .unwrap_or_else(|| "unknown failure".to_string())
}

fn truncate_for_prompt(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated = text.chars().take(max_chars).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn parsed_completion_claim_without_verification(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    [
        "done",
        "fixed",
        "complete",
        "completed",
        "resolved",
        "implemented",
        "완료",
        "수정했",
        "고쳤",
        "끝났",
        "해결",
        "구현했",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn parse_audit_limit(raw: &str, default: usize) -> usize {
    raw.parse::<usize>()
        .ok()
        .filter(|limit| *limit > 0)
        .map(|limit| limit.min(100))
        .unwrap_or(default)
}

fn modal_event(title: impl Into<String>, content: impl Into<String>) -> RuntimeEvent {
    RuntimeEvent::Modal {
        title: title.into(),
        content: content.into(),
    }
}

fn provider_modal_title(provider: &str) -> String {
    format!("{} Connection", provider_display_name(provider))
}

fn provider_display_name(provider: &str) -> &'static str {
    match provider {
        "openai" => "OpenAI",
        "openai-codex" => "OpenAI Codex",
        "anthropic" => "Anthropic",
        "google" => "Google Gemini",
        "ollama" => "Ollama",
        "openrouter" => "OpenRouter",
        _ => "Provider",
    }
}

fn provider_connection_content(provider: &str) -> String {
    match provider {
        "openai" => {
            "Connect OpenAI inside the TUI with:\n  /provider connect openai\n\nPaste an OpenAI API key when prompted. Charm saves it to ~/.charm/auth.json.\n\nSwitch with:\n  /model openai/gpt-4.1".to_string()
        }
        "openai-codex" => {
            "Connect OpenAI Codex inside the TUI with:\n  /provider connect openai-codex\n\nPaste a Codex access token when prompted. Charm saves it to ~/.charm/auth.json. Existing Codex login is still reused when present.\n\nSwitch with:\n  /model openai-codex/gpt-5.1-codex".to_string()
        }
        "anthropic" => {
            "Connect Anthropic inside the TUI with:\n  /provider connect anthropic\n\nPaste an Anthropic API key when prompted. Charm saves it to ~/.charm/auth.json.\n\nSwitch with:\n  /model anthropic/claude-sonnet-4-20250514".to_string()
        }
        "google" => {
            "Connect Google Gemini inside the TUI with:\n  /provider connect google\n\nPaste a Gemini API key when prompted. Charm saves it to ~/.charm/auth.json.\n\nSwitch with:\n  /model google/gemini-2.5-pro".to_string()
        }
        "ollama" => {
            "Run Ollama locally, then choose an installed model.\n\nExample:\n  ollama serve\n  ollama pull qwen3-coder:30b\n\nSwitch with:\n  /model ollama/qwen3-coder:30b".to_string()
        }
        "openrouter" => {
            "Connect OpenRouter inside the TUI with:\n  /provider connect openrouter\n\nPaste an OpenRouter API key when prompted. Charm saves it to ~/.charm/auth.json.\n\nSwitch with:\n  /model openrouter/moonshotai/kimi-k2.6".to_string()
        }
        other => format!(
            "Unknown provider `{other}`.\n\nKnown providers: openrouter, openai, openai-codex, anthropic, google, ollama."
        ),
    }
}

#[derive(Debug, Clone)]
struct RepeatedFailureInsight {
    tool_name: String,
    signature: String,
    count: usize,
    log_refs: Vec<String>,
}

#[derive(Debug, Default)]
struct TraceInsights {
    repeated_failures: Vec<RepeatedFailureInsight>,
    failed_tools: usize,
    policy_blocks: usize,
    verification_gaps: usize,
    reference_events: usize,
    missing_reference_risk: bool,
}

#[derive(Debug, Default)]
struct CandidateInsights {
    workflows: Vec<String>,
    rules: Vec<String>,
    memories: Vec<String>,
}

impl TraceInsights {
    fn candidates(&self) -> CandidateInsights {
        let mut candidates = CandidateInsights::default();
        if !self.repeated_failures.is_empty() {
            candidates.workflows.push(
                "After the same command fails twice, inspect the full log_ref and search precedents before retrying edits.".to_string(),
            );
        }
        if self.policy_blocks > 0 {
            candidates.rules.push(
                "Before mutating files, gather repo evidence for the exact target scope."
                    .to_string(),
            );
        }
        if self.verification_gaps > 0 {
            candidates.rules.push(
                "Do not claim completion until the verification gate has observed a passing check."
                    .to_string(),
            );
        }
        if self.missing_reference_risk {
            candidates.memories.push(
                "Repeated tool/API failures should trigger a reference pack or external precedent search before another implementation attempt.".to_string(),
            );
        }
        candidates
    }
}

#[derive(Debug, Default)]
struct FailureCluster {
    tool_name: String,
    signature: String,
    count: usize,
    log_refs: Vec<String>,
}

fn analyze_trace_insights(entries: &[TraceEntry]) -> TraceInsights {
    let mut insights = TraceInsights::default();
    let mut clusters = BTreeMap::<String, FailureCluster>::new();

    for entry in entries {
        match entry.event.as_str() {
            "tool_result" => {
                if !entry
                    .payload
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
                {
                    insights.failed_tools += 1;
                    let tool_name = entry
                        .payload
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown_tool")
                        .to_string();
                    let signature = failure_signature_from_trace_payload(&entry.payload);
                    let key = format!("{tool_name}\n{signature}");
                    let cluster = clusters.entry(key).or_insert_with(|| FailureCluster {
                        tool_name,
                        signature,
                        count: 0,
                        log_refs: Vec::new(),
                    });
                    cluster.count += 1;
                    if let Some(log_ref) = trace_payload_log_ref(&entry.payload)
                        && !cluster.log_refs.iter().any(|existing| existing == log_ref)
                    {
                        cluster.log_refs.push(log_ref.to_string());
                    }
                }
            }
            "tool_policy_blocked" => insights.policy_blocks += 1,
            "verification_gap" => insights.verification_gaps += 1,
            "reference_gate" | "external_precedent_required" => insights.reference_events += 1,
            _ => {}
        }
    }

    insights.repeated_failures = clusters
        .into_values()
        .filter(|cluster| cluster.count >= 2)
        .map(|cluster| RepeatedFailureInsight {
            tool_name: cluster.tool_name,
            signature: cluster.signature,
            count: cluster.count,
            log_refs: cluster.log_refs,
        })
        .collect();
    insights.repeated_failures.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.tool_name.cmp(&b.tool_name))
            .then_with(|| a.signature.cmp(&b.signature))
    });
    insights.missing_reference_risk = insights.failed_tools >= 2 && insights.reference_events == 0;
    insights
}

fn failure_signature_from_trace_payload(payload: &Value) -> String {
    ["error", "output", "minified_output"]
        .iter()
        .filter_map(|field| payload.get(field).and_then(Value::as_str))
        .flat_map(str::lines)
        .find_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed == "---stderr---" {
                None
            } else {
                Some(truncate_for_prompt(trimmed, 180))
            }
        })
        .or_else(|| {
            payload
                .get("metadata")
                .and_then(|metadata| metadata.get("output_hash"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown failure".to_string())
}

fn trace_payload_log_ref(payload: &Value) -> Option<&str> {
    payload
        .get("metadata")
        .and_then(|metadata| metadata.get("log_ref"))
        .and_then(Value::as_str)
}

fn format_trace_entry(entry: &TraceEntry) -> String {
    let turn = entry
        .turn_id
        .as_deref()
        .map(short_trace_id)
        .unwrap_or("no-turn");
    format!(
        "  {} {} [{}] {}",
        entry.timestamp.format("%H:%M:%S"),
        entry.event,
        turn,
        summarize_trace_payload(&entry.payload)
    )
}

fn short_trace_id(id: &str) -> &str {
    &id[..id.len().min(8)]
}

fn summarize_trace_payload(payload: &Value) -> String {
    if let Some(tool) = payload.get("tool_name").and_then(Value::as_str) {
        if let Some(success) = payload.get("success").and_then(Value::as_bool) {
            return format!("tool={tool} success={success}");
        }
        return format!("tool={tool}");
    }
    if let Some(reason) = payload.get("reason").and_then(Value::as_str) {
        return format!("reason={}", truncate_for_prompt(reason, 120));
    }
    if let Some(command) = payload.get("command").and_then(Value::as_str) {
        return format!("command={}", truncate_for_prompt(command, 120));
    }
    truncate_for_prompt(&serde_json::to_string(payload).unwrap_or_default(), 160)
}

fn source_kind_for_tool(call: &ToolCall) -> SourceKind {
    match call {
        ToolCall::RunCommand { command, .. } if command.contains("cargo test") => {
            SourceKind::TestOutput
        }
        ToolCall::RunCommand { command, .. }
            if command.contains("cargo check")
                || command.contains("cargo build")
                || command.contains("cargo clippy") =>
        {
            SourceKind::CargoOutput
        }
        ToolCall::RunCommand { .. }
        | ToolCall::PollCommand { .. }
        | ToolCall::CancelCommand { .. } => SourceKind::CommandOutput,
        ToolCall::GrepSearch { .. }
        | ToolCall::GlobSearch { .. }
        | ToolCall::ParallelSearch { .. } => SourceKind::SearchResults,
        ToolCall::ReadRange { .. } | ToolCall::ReadSymbol { .. } => SourceKind::CodeSnippet,
        _ => SourceKind::CommandOutput,
    }
}

fn requires_repo_evidence_before_execution(call: &ToolCall) -> bool {
    matches!(
        call,
        ToolCall::EditPatch { .. } | ToolCall::WriteFile { .. }
    )
}

fn tool_scope_targets(call: &ToolCall) -> Vec<String> {
    match call {
        ToolCall::EditPatch { file_path, .. } | ToolCall::WriteFile { file_path, .. } => {
            vec![file_path.clone()]
        }
        ToolCall::RunCommand {
            command,
            cwd: Some(cwd),
            risk_class:
                RiskClass::StatefulExec | RiskClass::Destructive | RiskClass::ExternalSideEffect,
            ..
        } if cwd != "." => {
            let mut targets = vec![cwd.clone()];
            for target in command_path_mentions(command) {
                let target = if Path::new(&target).is_relative() {
                    format!("{}/{}", cwd.trim_end_matches('/'), target)
                } else {
                    target
                };
                push_unique_string(&mut targets, target);
            }
            targets
        }
        ToolCall::RunCommand {
            command,
            risk_class:
                RiskClass::StatefulExec | RiskClass::Destructive | RiskClass::ExternalSideEffect,
            ..
        } => command_path_mentions(command),
        _ => Vec::new(),
    }
}

fn command_path_mentions(command: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for raw in command.split_whitespace() {
        let token = raw.trim_matches(|c: char| {
            matches!(
                c,
                '`' | '\''
                    | '"'
                    | ','
                    | ';'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '<'
                    | '>'
                    | '|'
                    | '&'
            )
        });
        if token.is_empty() || token.starts_with('-') {
            continue;
        }
        if let Some((_, value)) = token.split_once('=') {
            push_command_path_candidate(&mut paths, value);
        } else {
            push_command_path_candidate(&mut paths, token);
        }
    }
    paths
}

fn push_command_path_candidate(paths: &mut Vec<String>, candidate: &str) {
    let path = candidate
        .trim()
        .trim_start_matches("./")
        .trim_matches('"')
        .trim_matches('\'')
        .trim_end_matches(',')
        .trim_end_matches(';');
    if looks_like_command_path(path) {
        push_unique_string(paths, path.to_string());
    }
}

fn looks_like_command_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with("http://") || path.starts_with("https://") {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    path.starts_with('/')
        || lower.starts_with("src/")
        || lower.starts_with("docs/")
        || lower.starts_with("tests/")
        || lower.starts_with(".charm/")
        || [
            ".rs", ".md", ".toml", ".json", ".yaml", ".yml", ".py", ".js", ".ts", ".tsx", ".jsx",
            ".css", ".html", ".lock",
        ]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

fn push_unique_string(items: &mut Vec<String>, value: String) {
    if !items.iter().any(|item| item == &value) {
        items.push(value);
    }
}

fn concrete_scope_patterns(contract: &TaskContract) -> Vec<String> {
    contract
        .scope
        .iter()
        .filter_map(|scope| normalize_scope_pattern(scope))
        .collect()
}

fn normalize_scope_pattern(scope: &str) -> Option<String> {
    let trimmed = scope
        .trim()
        .trim_matches('`')
        .trim_start_matches("./")
        .trim_end_matches('/');
    if trimmed.is_empty()
        || trimmed.contains(' ')
        || trimmed.contains("determined")
        || trimmed.contains("Conservative")
    {
        return None;
    }
    if !(trimmed.contains('/') || trimmed.contains('.') || trimmed.ends_with("**")) {
        return None;
    }
    Some(trimmed.to_string())
}

fn scope_allows_target(target: &str, allowed_scope: &[String]) -> bool {
    allowed_scope
        .iter()
        .any(|scope| scope_pattern_allows_target(scope, target))
}

fn scope_pattern_allows_target(scope: &str, target: &str) -> bool {
    if let Some(base) = scope.strip_suffix("/**") {
        return target == base || target.starts_with(&format!("{base}/"));
    }
    if scope.ends_with('/') {
        return target.starts_with(scope);
    }
    if Path::new(scope).extension().is_some() {
        return target == scope;
    }
    target == scope || target.starts_with(&format!("{scope}/"))
}

fn path_to_slash(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn sanitize_export_filename(input: &str) -> String {
    let sanitized = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "export".to_string()
    } else {
        sanitized
    }
}

fn tool_provides_repo_evidence(call: &ToolCall) -> bool {
    matches!(
        call,
        ToolCall::ReadRange { .. }
            | ToolCall::ReadSymbol { .. }
            | ToolCall::GrepSearch { .. }
            | ToolCall::GlobSearch { .. }
            | ToolCall::ListDir { .. }
            | ToolCall::SemanticSearch { .. }
            | ToolCall::ParallelSearch { .. }
    )
}

fn tool_can_run_in_parallel_batch(call: &ToolCall) -> bool {
    matches!(
        call,
        ToolCall::ReadRange { .. }
            | ToolCall::ReadSymbol { .. }
            | ToolCall::GrepSearch { .. }
            | ToolCall::GlobSearch { .. }
            | ToolCall::ListDir { .. }
            | ToolCall::SemanticSearch { .. }
            | ToolCall::ParallelSearch { .. }
    )
}

fn tool_is_ordered_mutation(call: &ToolCall) -> bool {
    matches!(
        call,
        ToolCall::EditPatch { .. }
            | ToolCall::WriteFile { .. }
            | ToolCall::PlanUpdate { .. }
            | ToolCall::CheckpointCreate { .. }
            | ToolCall::CheckpointRestore { .. }
            | ToolCall::MemoryStage { .. }
            | ToolCall::MemoryCommit { .. }
            | ToolCall::RunCommand {
                risk_class: RiskClass::StatefulExec
                    | RiskClass::Destructive
                    | RiskClass::ExternalSideEffect,
                ..
            }
    )
}

fn mutation_barrier_reason(
    call: &ToolCall,
    tool_name: &str,
    result: &ToolResult,
) -> Option<String> {
    if !tool_is_ordered_mutation(call) {
        return None;
    }
    if result
        .metadata
        .as_ref()
        .and_then(|meta| meta.get("running"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some(format!(
            "previous mutating tool is still running: {tool_name}"
        ));
    }
    if !result.success {
        return Some(format!(
            "previous mutating tool failed or was blocked: {tool_name}"
        ));
    }
    None
}

fn evidence_queries_for_message(message: &str) -> Vec<String> {
    let mut queries = Vec::new();
    let trimmed = message.trim();
    if !trimmed.is_empty() {
        queries.push(trimmed.to_string());
    }

    for token in trimmed.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')) {
        let token = token.trim_matches('-');
        if token.len() < 4 || is_low_signal_query_token(token) {
            continue;
        }
        if !queries.iter().any(|existing| existing == token) {
            queries.push(token.to_string());
        }
    }

    queries
}

fn is_low_signal_query_token(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "fix"
            | "add"
            | "use"
            | "using"
            | "update"
            | "change"
            | "client"
            | "api"
            | "sdk"
            | "test"
            | "tests"
            | "current"
            | "state"
            | "with"
            | "from"
            | "into"
            | "the"
    )
}

fn reference_gate_required(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        " api",
        "sdk",
        "dependency",
        "dependencies",
        "crate",
        "package",
        "library",
        "upgrade",
        "migrate",
        "version",
        "docs",
        "documentation",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn reference_packages_for_message(message: &str, packages: Vec<PackageId>) -> Vec<PackageId> {
    let lower = message.to_ascii_lowercase();
    let mut matched = packages
        .iter()
        .filter(|package| lower.contains(&package.name.to_ascii_lowercase()))
        .cloned()
        .collect::<Vec<_>>();

    if matched.is_empty() && reference_gate_required(message) {
        matched = packages.into_iter().take(3).collect();
    }

    matched
}

fn workspace_package_name(workspace_root: &Path) -> String {
    workspace_root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("workspace")
        .to_string()
}

fn local_reference_source_roots(workspace_root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![
        workspace_root.join(".cargo").join("registry").join("src"),
        workspace_root.join("vendor"),
    ];

    if let Ok(cargo_home) = std::env::var("CARGO_HOME") {
        roots.push(PathBuf::from(cargo_home).join("registry").join("src"));
    }
    if let Ok(home) = std::env::var("HOME") {
        roots.push(
            PathBuf::from(home)
                .join(".cargo")
                .join("registry")
                .join("src"),
        );
    }

    roots
}

fn local_reference_gate_pack(
    broker: &ReferenceBroker,
    message: &str,
    package: PackageId,
) -> ReferencePack {
    let mut pack = broker.compile_reference_pack(
        ReferenceSourceKind::LocalSource,
        vec![RawFinding {
            kind: FindingKind::Caveat,
            title: Some("Reference gate required".to_string()),
            content: format!(
                "Do not rely on model memory alone for {}. Resolve installed source or official docs before implementing API-specific behavior.",
                package.name
            ),
            language: None,
            source_url: None,
        }],
        message,
    );
    pack.library = Some(package.name);
    pack.version = package.version;
    pack.confidence = ReferenceConfidence::Uncertain;
    pack.relevant_rules.push(
        "Reference-first: verify package APIs with local source, docs MCP, or official docs."
            .to_string(),
    );
    pack
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
  /compact               Roll old turns into a TokenSaver-backed summary
  /audit                 Show trace counts, policy blocks, and failures
  /audit insights [n]    Analyze repeated failures and suggest rules/workflows
  /audit replay [n]      Replay recent trace events
  /evidence [repo|refs]  Browse persisted repo evidence and reference packs
  /clear                 Clear transcript (keep system prompt)
  /model <id>            Pin a model for this session
  /session [next|prev|<id>]  Rotate between sessions
  /agent spawn <task>    Start a background sub-agent
  /agent list|diff <id>  Inspect sub-agent output
  /agent export <id>     Export sub-agent review artifact
  /agent pr <id>         Write a local PR draft artifact
  /agent merge|cleanup <id>  Apply or remove sub-agent worktree
  /agent kill <id>      Cancel a sub-agent
  /approvals             Show pending approvals
  /approvals approve <id>  Approve a pending tool request
  /approvals deny <id>  Deny a pending tool request
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
  Ctrl+Shift+A    Approval queue
  Ctrl+Tab        Next session
  Ctrl+Shift+Tab  Previous session
  Ctrl+B / Ctrl+D Toggle left / right docks
  Tab             Autocomplete slash command (completes common prefix)
  Shift+Enter     Insert newline in composer
  Option+Enter    Insert newline in composer
  Option+←/→      Move by word (Alt/Meta fallback)
  Option+Backspace/Delete  Delete word backward/forward
  ↑ / ↓           Navigate slash dropdown / overlay list
  F1 / ?          Open this help overlay
  PgUp / PgDn     Scroll transcript page-by-page
  Shift+Up/Down   Fine-grain scroll (disengages auto-follow)
  Mouse wheel     Scroll transcript (wheel-down at bottom re-engages follow)
  Esc             Dismiss overlay, clear draft, then quit

Autonomy levels (see docs/charm-strategy.md § Autonomy Profiles)
  conservative  Every write/exec needs approval. Reads auto.
  balanced      Reads + safe exec auto; stateful work asks.
  aggressive    Reads, searches, edits, tests auto; destructive asks.
  yolo          All tools auto-approved. Destructive ops still log a ⚠ trace
                line. Use git stash/checkpoint before risky runs.

Coming soon
  • Auto model routing via `charm delegate` (Planner/Worker).
  • Remote GitHub PR publishing from reviewed sub-agent output.
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
        current_task_contract: None,
        verification: VerificationState::default(),
        repo_evidence: Vec::new(),
        reference_packs: Vec::new(),
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
        ToolCall::CancelCommand { .. } => "cancel_command",
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

    struct FailingModel;

    #[async_trait]
    impl RuntimeModel for FailingModel {
        async fn chat(&self, _request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)> {
            Err(anyhow::anyhow!("provider unavailable"))
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamChunk>>> {
            Err(anyhow::anyhow!("stream unavailable"))
        }

        fn tool_schemas(&self) -> Vec<ToolSchema> {
            crate::providers::types::default_tool_schemas()
        }
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
    async fn submit_input_concretizes_turn_and_injects_contract_into_system_prompt() {
        let dir = tempdir().unwrap();
        let model = fake_model(vec![Message {
            role: "assistant".to_string(),
            content: Some("Done".to_string()),
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

        runtime
            .submit_input("Fix src/main.rs panic and run tests")
            .await
            .unwrap();

        let contract = runtime
            .snapshot()
            .current_task_contract
            .as_ref()
            .expect("turn should have a concretized task contract");
        assert_eq!(contract.objective, "Fix src/main.rs panic and run tests");
        assert!(
            contract
                .verification
                .iter()
                .any(|item| item.contains("Build") || item.contains("test"))
        );

        let system = runtime.snapshot().messages[0]
            .content
            .as_ref()
            .expect("system prompt");
        assert!(system.contains("## Current Task Contract"));
        assert!(system.contains("Fix src/main.rs panic and run tests"));
        assert!(system.contains("## Verification Gate"));
    }

    #[tokio::test]
    async fn submit_input_persists_turn_contract_and_model_trace() {
        let dir = tempdir().unwrap();
        let model = fake_model(vec![Message {
            role: "assistant".to_string(),
            content: Some("Traceable response".to_string()),
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

        runtime.submit_input("Explain the repo").await.unwrap();

        let trace_path = dir
            .path()
            .join(".charm")
            .join("traces")
            .join(format!("{session_id}.jsonl"));
        let trace = std::fs::read_to_string(trace_path).expect("trace jsonl");
        assert!(trace.contains("\"event\":\"task_contract\""));
        assert!(trace.contains("\"event\":\"model_response\""));
        assert!(trace.contains("Traceable response"));
    }

    #[tokio::test]
    async fn successful_command_tool_updates_verification_state_and_trace() {
        let dir = tempdir().unwrap();
        let model = fake_model(vec![
            Message {
                role: "assistant".to_string(),
                content: Some("Running verification".to_string()),
                tool_calls: Some(vec![ToolCallBlock {
                    id: "call-verify".to_string(),
                    r#type: "function".to_string(),
                    function: FunctionCall {
                        name: "run_command".to_string(),
                        arguments: serde_json::json!({
                            "command": "printf 'verification ok\\n'",
                            "risk_class": "safe"
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
                content: Some("Verified".to_string()),
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
        let session_id = runtime.snapshot().metadata.session_id.clone();

        runtime
            .submit_input("Verify the current state")
            .await
            .unwrap();

        assert!(
            runtime
                .snapshot()
                .verification
                .observed
                .iter()
                .any(|item| item.contains("printf 'verification ok"))
        );
        assert!(runtime.snapshot().verification.satisfied);

        let trace_path = dir
            .path()
            .join(".charm")
            .join("traces")
            .join(format!("{session_id}.jsonl"));
        let trace = std::fs::read_to_string(trace_path).expect("trace jsonl");
        assert!(trace.contains("\"event\":\"tool_result\""));
        assert!(trace.contains("\"minified_output\""));
        assert!(trace.contains("\"raw_ref\""));
        assert!(trace.contains("verification ok"));
    }

    #[tokio::test]
    async fn edit_tool_is_blocked_until_repo_evidence_is_observed() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("main.rs"), "fn main() {}\n").unwrap();
        let model = fake_model(vec![
            Message {
                role: "assistant".to_string(),
                content: Some("Editing directly".to_string()),
                tool_calls: Some(vec![ToolCallBlock {
                    id: "call-edit".to_string(),
                    r#type: "function".to_string(),
                    function: FunctionCall {
                        name: "edit_patch".to_string(),
                        arguments: serde_json::json!({
                            "file_path": "src/main.rs",
                            "old_string": "fn main() {}\n",
                            "new_string": "fn main() { println!(\"hi\"); }\n"
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
                content: Some("Need to inspect first".to_string()),
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
        let session_id = runtime.snapshot().metadata.session_id.clone();

        runtime.submit_input("Adjust greeting").await.unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("src").join("main.rs")).unwrap(),
            "fn main() {}\n"
        );
        let tool_message = runtime
            .snapshot()
            .messages
            .iter()
            .find(|message| message.role == "tool")
            .and_then(|message| message.content.as_ref())
            .expect("blocked tool result");
        assert!(tool_message.contains("Tool policy gate"));

        let trace_path = dir
            .path()
            .join(".charm")
            .join("traces")
            .join(format!("{session_id}.jsonl"));
        let trace = std::fs::read_to_string(trace_path).expect("trace jsonl");
        assert!(trace.contains("\"event\":\"tool_policy_blocked\""));
    }

    #[tokio::test]
    async fn submit_input_collects_repo_evidence_before_model_call() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src").join("main.rs"),
            "fn panic_path() { panic!(\"boom\"); }\n",
        )
        .unwrap();
        let model = fake_model(vec![Message {
            role: "assistant".to_string(),
            content: Some("I saw repo evidence".to_string()),
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

        runtime.submit_input("Fix panic_path").await.unwrap();

        assert!(
            runtime
                .snapshot()
                .repo_evidence
                .iter()
                .any(|item| item.file_path.contains("src/main.rs"))
        );
        let system = runtime.snapshot().messages[0]
            .content
            .as_ref()
            .expect("system prompt");
        assert!(system.contains("## Repo Evidence"));
        assert!(system.contains("panic_path"));

        let trace_path = dir
            .path()
            .join(".charm")
            .join("traces")
            .join(format!("{session_id}.jsonl"));
        let trace = std::fs::read_to_string(trace_path).expect("trace jsonl");
        assert!(trace.contains("\"event\":\"repo_evidence\""));
    }

    #[tokio::test]
    async fn external_api_request_adds_reference_gate_pack_to_prompt() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='demo'\nversion='0.1.0'\n\n[dependencies]\nreqwest = \"0.12\"\n",
        )
        .unwrap();
        let source_dir = dir
            .path()
            .join(".cargo")
            .join("registry")
            .join("src")
            .join("index")
            .join("reqwest-0.12");
        std::fs::create_dir_all(source_dir.join("src")).unwrap();
        std::fs::write(
            source_dir.join("README.md"),
            "# reqwest\n\nUse Client::new() for HTTP clients.\n",
        )
        .unwrap();
        let model = fake_model(vec![Message {
            role: "assistant".to_string(),
            content: Some("Need docs".to_string()),
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

        runtime
            .submit_input("Use the reqwest API to add a client")
            .await
            .unwrap();

        assert!(
            runtime
                .snapshot()
                .reference_packs
                .iter()
                .any(|pack| pack.library.as_deref() == Some("reqwest"))
        );
        let system = runtime.snapshot().messages[0]
            .content
            .as_ref()
            .expect("system prompt");
        assert!(system.contains("## Reference Gate"));
        assert!(system.contains("reqwest"));
        assert!(system.contains("Client::new"));

        let trace_path = dir
            .path()
            .join(".charm")
            .join("traces")
            .join(format!("{session_id}.jsonl"));
        let trace = std::fs::read_to_string(trace_path).expect("trace jsonl");
        assert!(trace.contains("\"event\":\"reference_gate\""));
    }

    #[tokio::test]
    async fn completion_claim_without_verification_emits_gap_event() {
        let dir = tempdir().unwrap();
        let model = fake_model(vec![Message {
            role: "assistant".to_string(),
            content: Some("Done, fixed and complete.".to_string()),
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

        let events = runtime.submit_input("Fix the issue").await.unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::MessageDelta { role, content }
                if role == "system" && content.contains("Verification gate not satisfied")
        )));
        assert!(
            runtime
                .snapshot()
                .verification
                .last_status
                .as_ref()
                .is_some_and(|status| status.contains("completion claim blocked"))
        );

        let trace_path = dir
            .path()
            .join(".charm")
            .join("traces")
            .join(format!("{session_id}.jsonl"));
        let trace = std::fs::read_to_string(trace_path).expect("trace jsonl");
        assert!(trace.contains("\"event\":\"verification_gap\""));
    }

    #[tokio::test]
    async fn repeated_command_failures_force_external_precedent_pack() {
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
        runtime.reference_broker = ReferenceBroker::new().with_http_get(|url| async move {
            assert!(url.starts_with("https://api.github.com/search/issues?q="));
            Ok(r#"{
  "total_count": 1,
  "items": [
    {
      "title": "Known unresolved import fix",
      "html_url": "https://github.com/example/project/issues/42",
      "state": "closed",
      "score": 7.25
    }
  ]
}"#
            .to_string())
        });
        let session_id = runtime.snapshot().metadata.session_id.clone();
        runtime.current_turn_id = Some("turn-precedent".to_string());

        let call = ToolCall::RunCommand {
            command: "cargo test".to_string(),
            cwd: None,
            blocking: true,
            timeout_ms: None,
            risk_class: RiskClass::SafeExec,
        };
        let failure = ToolResult {
            success: false,
            output: "error[E0432]: unresolved import `demo`\nfailed to compile".to_string(),
            error: Some("cargo test failed".to_string()),
            metadata: Some(serde_json::json!({ "exit_code": 101 })),
        };

        runtime
            .record_tool_result(&call, "run_command", &failure)
            .await
            .unwrap();
        assert!(runtime.snapshot().reference_packs.is_empty());

        runtime
            .record_tool_result(&call, "run_command", &failure)
            .await
            .unwrap();

        assert!(runtime.snapshot().reference_packs.iter().any(|pack| {
            pack.source_kind == ReferenceSourceKind::GitHubIssues
                && pack.query.contains("cargo test")
                && pack
                    .relevant_rules
                    .iter()
                    .any(|rule| rule.contains("stop guessing"))
                && pack
                    .source_refs
                    .iter()
                    .any(|source| source.url == "https://github.com/example/project/issues/42")
        }));

        let trace_path = dir
            .path()
            .join(".charm")
            .join("traces")
            .join(format!("{session_id}.jsonl"));
        let trace = std::fs::read_to_string(trace_path).expect("trace jsonl");
        assert!(trace.contains("\"event\":\"external_precedent_required\""));
    }

    #[tokio::test]
    async fn repeated_command_failures_include_github_discussion_precedents() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"

[dependencies]
tokio = "1.44"
"#,
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
        runtime.reference_broker = ReferenceBroker::new()
            .with_http_get(|url| async move {
                if url.starts_with("https://api.github.com/search/issues?q=") {
                    return Ok(r#"{ "total_count": 0, "items": [] }"#.to_string());
                }
                assert_eq!(url, "https://crates.io/api/v1/crates/tokio");
                Ok(r#"{
  "crate": {
    "id": "tokio",
    "max_version": "1.44.0",
    "repository": "https://github.com/tokio-rs/tokio"
  }
}"#
                .to_string())
            })
            .with_http_post(|url, body| async move {
                assert_eq!(url, "https://api.github.com/graphql");
                assert!(body.contains("tokio-rs"));
                assert!(body.contains("tokio"));
                Ok(r#"{
  "data": {
    "repository": {
      "discussions": {
        "nodes": [
          {
            "title": "unresolved import after feature flags",
            "url": "https://github.com/tokio-rs/tokio/discussions/7",
            "bodyText": "error[E0432] unresolved import tokio::runtime",
            "isAnswered": true,
            "answer": { "bodyText": "enable the full feature", "url": "https://github.com/tokio-rs/tokio/discussions/7#discussioncomment-1" }
          }
        ]
      }
    }
  }
}"#
                .to_string())
            });
        runtime.current_turn_id = Some("turn-discussion-precedent".to_string());

        let call = ToolCall::RunCommand {
            command: "cargo test".to_string(),
            cwd: None,
            blocking: true,
            timeout_ms: None,
            risk_class: RiskClass::SafeExec,
        };
        let failure = ToolResult {
            success: false,
            output: "error[E0432]: unresolved import tokio::runtime\nfailed to compile".to_string(),
            error: None,
            metadata: Some(serde_json::json!({ "exit_code": 101 })),
        };

        runtime
            .record_tool_result(&call, "run_command", &failure)
            .await
            .unwrap();
        runtime
            .record_tool_result(&call, "run_command", &failure)
            .await
            .unwrap();

        assert!(runtime.snapshot().reference_packs.iter().any(|pack| {
            pack.source_kind == ReferenceSourceKind::GitHubIssues
                && pack
                    .source_refs
                    .iter()
                    .any(|source| source.url == "https://github.com/tokio-rs/tokio/discussions/7")
                && pack
                    .caveats
                    .iter()
                    .any(|caveat| caveat.contains("Answered discussion matched"))
        }));
    }

    #[tokio::test]
    async fn read_only_tool_calls_run_as_parallel_batch_with_ordered_results() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("a.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(dir.path().join("src").join("b.rs"), "pub fn b() {}\n").unwrap();
        let model = fake_model(vec![
            Message {
                role: "assistant".to_string(),
                content: Some("Inspecting both files".to_string()),
                tool_calls: Some(vec![
                    ToolCallBlock {
                        id: "call-a".to_string(),
                        r#type: "function".to_string(),
                        function: FunctionCall {
                            name: "read_range".to_string(),
                            arguments: serde_json::json!({
                                "file_path": "src/a.rs",
                                "offset": 0,
                                "limit": 10
                            })
                            .to_string(),
                        },
                    },
                    ToolCallBlock {
                        id: "call-b".to_string(),
                        r#type: "function".to_string(),
                        function: FunctionCall {
                            name: "read_range".to_string(),
                            arguments: serde_json::json!({
                                "file_path": "src/b.rs",
                                "offset": 0,
                                "limit": 10
                            })
                            .to_string(),
                        },
                    },
                ]),
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            },
            Message {
                role: "assistant".to_string(),
                content: Some("Done".to_string()),
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
        let session_id = runtime.snapshot().metadata.session_id.clone();

        let events = runtime.submit_input("Read both files").await.unwrap();

        let tool_event_order = events
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::ToolCallStarted { .. } => Some("started"),
                RuntimeEvent::ToolCallFinished { .. } => Some("finished"),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            tool_event_order,
            vec!["started", "started", "finished", "finished"]
        );
        let tool_call_ids = runtime
            .snapshot()
            .messages
            .iter()
            .filter(|message| message.role == "tool")
            .filter_map(|message| message.tool_call_id.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(tool_call_ids, vec!["call-a", "call-b"]);

        let trace_path = dir
            .path()
            .join(".charm")
            .join("traces")
            .join(format!("{session_id}.jsonl"));
        let trace = std::fs::read_to_string(trace_path).expect("trace jsonl");
        assert!(trace.contains("\"event\":\"parallel_tool_batch\""));
        assert!(trace.contains("\"tool_count\":2"));
    }

    #[tokio::test]
    async fn mixed_tool_calls_parallelize_read_prefix_before_ordered_write() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("a.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(dir.path().join("src").join("b.rs"), "pub fn b() {}\n").unwrap();
        let model = fake_model(vec![
            Message {
                role: "assistant".to_string(),
                content: Some("Inspecting then writing".to_string()),
                tool_calls: Some(vec![
                    ToolCallBlock {
                        id: "call-a".to_string(),
                        r#type: "function".to_string(),
                        function: FunctionCall {
                            name: "read_range".to_string(),
                            arguments: serde_json::json!({
                                "file_path": "src/a.rs",
                                "offset": 0,
                                "limit": 10
                            })
                            .to_string(),
                        },
                    },
                    ToolCallBlock {
                        id: "call-b".to_string(),
                        r#type: "function".to_string(),
                        function: FunctionCall {
                            name: "read_range".to_string(),
                            arguments: serde_json::json!({
                                "file_path": "src/b.rs",
                                "offset": 0,
                                "limit": 10
                            })
                            .to_string(),
                        },
                    },
                    ToolCallBlock {
                        id: "call-write".to_string(),
                        r#type: "function".to_string(),
                        function: FunctionCall {
                            name: "write_file".to_string(),
                            arguments: serde_json::json!({
                                "file_path": "src/output.rs",
                                "content": "pub fn output() {}\n"
                            })
                            .to_string(),
                        },
                    },
                ]),
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            },
            Message {
                role: "assistant".to_string(),
                content: Some("Done".to_string()),
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

        let events = runtime
            .submit_input("Read then write src files")
            .await
            .unwrap();

        let tool_event_order = events
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::ToolCallStarted { .. } => Some("started"),
                RuntimeEvent::ToolCallFinished { .. } => Some("finished"),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            tool_event_order,
            vec![
                "started", "started", "finished", "finished", "started", "finished"
            ]
        );
        let tool_call_ids = runtime
            .snapshot()
            .messages
            .iter()
            .filter(|message| message.role == "tool")
            .filter_map(|message| message.tool_call_id.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(tool_call_ids, vec!["call-a", "call-b", "call-write"]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src").join("output.rs")).unwrap(),
            "pub fn output() {}\n"
        );
    }

    #[tokio::test]
    async fn scope_guard_blocks_write_outside_current_task_contract() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/tui")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/core")).unwrap();
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
        runtime.snapshot.current_task_contract = Some(TaskContract {
            abstraction_score: 0.4,
            objective: "Fix TUI shortcuts".to_string(),
            scope: vec!["src/tui/**".to_string()],
            repo_anchors: Vec::new(),
            acceptance: Vec::new(),
            verification: Vec::new(),
            side_effects: vec!["May affect TUI input/keybinding compatibility".to_string()],
            assumptions: Vec::new(),
            open_questions: Vec::new(),
            depth: crate::agent::task_concretizer::ExecutionDepth::Normal,
        });
        runtime.turn_repo_evidence_seen = true;

        let result = runtime
            .execute_tool_with_gates(
                &ToolCall::WriteFile {
                    file_path: "src/core/mod.rs".to_string(),
                    content: "pub fn outside() {}\n".to_string(),
                },
                "write_file",
            )
            .await;

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("outside current task scope")
        );
        assert_eq!(
            result
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("blocked_by"))
                .and_then(Value::as_str),
            Some("scope_guard")
        );
        assert!(!dir.path().join("src/core/mod.rs").exists());
    }

    #[tokio::test]
    async fn scope_guard_allows_write_inside_current_task_contract() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/tui")).unwrap();
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
        runtime.snapshot.current_task_contract = Some(TaskContract {
            abstraction_score: 0.4,
            objective: "Fix TUI shortcuts".to_string(),
            scope: vec!["src/tui/**".to_string()],
            repo_anchors: Vec::new(),
            acceptance: Vec::new(),
            verification: Vec::new(),
            side_effects: vec!["May affect TUI input/keybinding compatibility".to_string()],
            assumptions: Vec::new(),
            open_questions: Vec::new(),
            depth: crate::agent::task_concretizer::ExecutionDepth::Normal,
        });
        runtime.turn_repo_evidence_seen = true;

        let result = runtime
            .execute_tool_with_gates(
                &ToolCall::WriteFile {
                    file_path: "src/tui/app.rs".to_string(),
                    content: "pub fn inside() {}\n".to_string(),
                },
                "write_file",
            )
            .await;

        assert!(result.success, "{result:?}");
        assert!(dir.path().join("src/tui/app.rs").exists());
    }

    #[tokio::test]
    async fn scope_guard_uses_repo_evidence_when_contract_scope_is_abstract() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/tui")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/core")).unwrap();
        std::fs::write(dir.path().join("src/tui/app.rs"), "pub fn tui() {}\n").unwrap();
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
        runtime.snapshot.current_task_contract = Some(TaskContract {
            abstraction_score: 0.5,
            objective: "Fix relevant code".to_string(),
            scope: vec!["Conservative scope - will expand after initial inspection".to_string()],
            repo_anchors: Vec::new(),
            acceptance: Vec::new(),
            verification: Vec::new(),
            side_effects: Vec::new(),
            assumptions: Vec::new(),
            open_questions: Vec::new(),
            depth: crate::agent::task_concretizer::ExecutionDepth::Normal,
        });
        runtime.snapshot.repo_evidence = vec![crate::retrieval::types::Evidence {
            source: "grep".to_string(),
            rank: 1.0,
            file_path: "src/tui/app.rs".to_string(),
            line: 1,
            snippet: "pub fn tui() {}".to_string(),
            context: None,
        }];
        runtime.turn_repo_evidence_seen = true;

        let result = runtime
            .execute_tool_with_gates(
                &ToolCall::WriteFile {
                    file_path: "src/core/mod.rs".to_string(),
                    content: "pub fn outside() {}\n".to_string(),
                },
                "write_file",
            )
            .await;

        assert!(!result.success);
        assert!(
            result
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("allowed_scope"))
                .and_then(Value::as_array)
                .is_some_and(|scope| scope.iter().any(|item| item == "src/tui/app.rs"))
        );
        assert!(!dir.path().join("src/core/mod.rs").exists());
    }

    #[tokio::test]
    async fn scope_guard_allows_repo_evidence_target_when_contract_scope_is_abstract() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/tui")).unwrap();
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
        runtime.snapshot.current_task_contract = Some(TaskContract {
            abstraction_score: 0.5,
            objective: "Fix relevant code".to_string(),
            scope: vec!["Conservative scope - will expand after initial inspection".to_string()],
            repo_anchors: Vec::new(),
            acceptance: Vec::new(),
            verification: Vec::new(),
            side_effects: Vec::new(),
            assumptions: Vec::new(),
            open_questions: Vec::new(),
            depth: crate::agent::task_concretizer::ExecutionDepth::Normal,
        });
        runtime.snapshot.repo_evidence = vec![crate::retrieval::types::Evidence {
            source: "grep".to_string(),
            rank: 1.0,
            file_path: "src/tui/app.rs".to_string(),
            line: 1,
            snippet: "pub fn tui() {}".to_string(),
            context: None,
        }];
        runtime.turn_repo_evidence_seen = true;

        let result = runtime
            .execute_tool_with_gates(
                &ToolCall::WriteFile {
                    file_path: "src/tui/app.rs".to_string(),
                    content: "pub fn tui() { println!(\"ok\"); }\n".to_string(),
                },
                "write_file",
            )
            .await;

        assert!(result.success, "{result:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/tui/app.rs")).unwrap(),
            "pub fn tui() { println!(\"ok\"); }\n"
        );
    }

    #[tokio::test]
    async fn scope_guard_blocks_stateful_command_target_outside_current_task_contract() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/tui")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/core")).unwrap();
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
        runtime.snapshot.current_task_contract = Some(TaskContract {
            abstraction_score: 0.4,
            objective: "Fix TUI shortcuts".to_string(),
            scope: vec!["src/tui/**".to_string()],
            repo_anchors: Vec::new(),
            acceptance: Vec::new(),
            verification: Vec::new(),
            side_effects: vec!["May affect TUI input/keybinding compatibility".to_string()],
            assumptions: Vec::new(),
            open_questions: Vec::new(),
            depth: crate::agent::task_concretizer::ExecutionDepth::Normal,
        });

        let result = runtime
            .execute_tool_with_gates(
                &ToolCall::RunCommand {
                    command: "touch src/core/outside.rs".to_string(),
                    cwd: None,
                    blocking: true,
                    timeout_ms: None,
                    risk_class: RiskClass::StatefulExec,
                },
                "run_command",
            )
            .await;

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("outside current task scope")
        );
        assert!(!dir.path().join("src/core/outside.rs").exists());
    }

    #[tokio::test]
    async fn scope_guard_allows_stateful_command_target_inside_current_task_contract() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/tui")).unwrap();
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
        runtime.snapshot.current_task_contract = Some(TaskContract {
            abstraction_score: 0.4,
            objective: "Fix TUI shortcuts".to_string(),
            scope: vec!["src/tui/**".to_string()],
            repo_anchors: Vec::new(),
            acceptance: Vec::new(),
            verification: Vec::new(),
            side_effects: vec!["May affect TUI input/keybinding compatibility".to_string()],
            assumptions: Vec::new(),
            open_questions: Vec::new(),
            depth: crate::agent::task_concretizer::ExecutionDepth::Normal,
        });

        let result = runtime
            .execute_tool_with_gates(
                &ToolCall::RunCommand {
                    command: "touch src/tui/inside.rs".to_string(),
                    cwd: None,
                    blocking: true,
                    timeout_ms: None,
                    risk_class: RiskClass::StatefulExec,
                },
                "run_command",
            )
            .await;

        assert!(result.success, "{result:?}");
        assert!(dir.path().join("src/tui/inside.rs").exists());
    }

    #[tokio::test]
    async fn compact_context_rolls_old_messages_into_summary() {
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

        for idx in 0..12 {
            runtime.snapshot.messages.push(Message {
                role: "user".to_string(),
                content: Some(format!("old request {idx}")),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            });
            runtime.snapshot.messages.push(Message {
                role: "assistant".to_string(),
                content: Some(format!("old decision {idx}")),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            });
        }

        let events = runtime.submit_input("/compact").await.unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::ContextCompacted {
                removed_messages,
                ..
            } if *removed_messages > 0
        )));
        assert_eq!(runtime.snapshot.messages[0].role, "system");
        assert!(
            runtime.snapshot.messages[1]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("[Earlier:"))
        );
        assert!(runtime.snapshot.messages.len() < 26);
    }

    #[tokio::test]
    async fn compact_context_preserves_minified_old_tool_evidence() {
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

        for idx in 0..12 {
            runtime.snapshot.messages.push(Message {
                role: "user".to_string(),
                content: Some(format!("old request {idx}")),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            });
            runtime.snapshot.messages.push(Message {
                role: "tool".to_string(),
                content: Some(
                    serde_json::to_string(&ToolResult {
                        success: true,
                        output: format!("src/lib.rs\n12: let important_{idx} = value;"),
                        error: None,
                        metadata: None,
                    })
                    .unwrap(),
                ),
                tool_calls: None,
                tool_call_id: Some(format!("tool-{idx}")),
                reasoning: None,
                reasoning_details: None,
            });
        }

        let events = runtime.submit_input("/compact").await.unwrap();

        assert!(
            events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::ContextCompacted { .. }))
        );
        let summary = runtime.snapshot.messages[1]
            .content
            .as_deref()
            .unwrap_or_default();
        assert!(summary.contains("TokenSaver evidence"));
        assert!(summary.contains("12: let important_0 = value;"));
    }

    #[tokio::test]
    async fn audit_command_renders_recent_trace_summary() {
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
        runtime
            .trace(
                Some("turn-audit"),
                "tool_result",
                serde_json::json!({
                    "tool_name": "run_command",
                    "success": true
                }),
            )
            .unwrap();
        runtime
            .trace(
                Some("turn-audit"),
                "tool_policy_blocked",
                serde_json::json!({
                    "reason": "scope guard"
                }),
            )
            .unwrap();

        let events = runtime.submit_input("/audit").await.unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Modal { content, .. }
                if content.contains("Audit") && content.contains("tool_result: 1") && content.contains("tool_policy_blocked: 1")
        )));
    }

    #[tokio::test]
    async fn help_and_evidence_commands_return_modal_events() {
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

        let help = runtime.submit_input("/help").await.unwrap();
        assert!(help.iter().any(|event| matches!(
            event,
            RuntimeEvent::Modal { title, content }
                if title == "Help" && content.contains("Slash commands")
        )));

        let evidence = runtime.submit_input("/evidence").await.unwrap();
        assert!(evidence.iter().any(|event| matches!(
            event,
            RuntimeEvent::Modal { title, content }
                if title == "Evidence" && content.contains("Evidence Browser")
        )));
    }

    #[tokio::test]
    async fn provider_connect_command_returns_connection_modal() {
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

        let events = runtime
            .submit_input("/provider connect openai")
            .await
            .unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Modal { title, content }
                if title.contains("OpenAI") && content.contains("~/.charm/auth.json")
        )));
    }

    #[tokio::test]
    async fn canonical_ollama_model_switch_rebinds_provider_without_warning() {
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

        let events = runtime
            .submit_input("/model ollama/qwen3-coder:30b")
            .await
            .unwrap();

        assert_eq!(runtime.model_name, "qwen3-coder:30b");
        assert_eq!(runtime.model_display(), "ollama/qwen3-coder:30b");
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::ModelChanged { model, display }
                if model == "qwen3-coder:30b" && display == "ollama/qwen3-coder:30b"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::MessageDelta { content, .. }
                if content.contains("Provider connected: ollama")
                    && !content.contains("reuses current provider client")
        )));
    }

    #[tokio::test]
    async fn audit_replay_command_renders_recent_trace_timeline() {
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
        runtime
            .trace(
                Some("turn-replay"),
                "task_contract",
                serde_json::json!({"objective": "audit"}),
            )
            .unwrap();
        runtime
            .trace(
                Some("turn-replay"),
                "verification_gap",
                serde_json::json!({"claim": "done"}),
            )
            .unwrap();

        let events = runtime.submit_input("/audit replay 2").await.unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Modal { content, .. }
                if content.contains("Trace Replay") && content.contains("task_contract") && content.contains("verification_gap")
        )));
    }

    #[tokio::test]
    async fn audit_insights_reports_repeated_failures_and_candidate_actions() {
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
        for log_ref in [".charm/logs/commands/a.log", ".charm/logs/commands/b.log"] {
            runtime
                .trace(
                    Some("turn-fail"),
                    "tool_result",
                    serde_json::json!({
                        "tool_name": "run_command",
                        "success": false,
                        "error": "error[E0432]: unresolved import crate::missing",
                        "metadata": {
                            "log_ref": log_ref,
                            "output_hash": "fnv1a64:abc"
                        }
                    }),
                )
                .unwrap();
        }
        runtime
            .trace(
                Some("turn-fail"),
                "tool_policy_blocked",
                serde_json::json!({
                    "metadata": {"blocked_by": "repo_evidence_gate"}
                }),
            )
            .unwrap();
        runtime
            .trace(
                Some("turn-fail"),
                "verification_gap",
                serde_json::json!({"claim": "done"}),
            )
            .unwrap();

        let events = runtime.submit_input("/audit insights").await.unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Modal { content, .. }
                if content.contains("Audit Insights")
                    && content.contains("Repeated failures")
                    && content.contains("unresolved import")
                    && content.contains(".charm/logs/commands/a.log")
                    && content.contains("Missing reference risk")
                    && content.contains("Candidate workflows")
                    && content.contains("Candidate rules")
        )));
    }

    #[tokio::test]
    async fn evidence_command_reads_persisted_session_evidence() {
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

        runtime.snapshot.repo_evidence = vec![crate::retrieval::types::Evidence {
            source: "grep".to_string(),
            rank: 0.91,
            file_path: "src/lib.rs".to_string(),
            line: 12,
            snippet: "pub fn persisted_evidence() {}".to_string(),
            context: None,
        }];
        runtime.snapshot.reference_packs = vec![ReferencePack {
            source_kind: ReferenceSourceKind::OfficialDocs,
            library: Some("reqwest".to_string()),
            version: Some("0.12".to_string()),
            query: "reqwest client".to_string(),
            relevant_rules: vec!["Use Client::new".to_string()],
            minimal_examples: Vec::new(),
            caveats: Vec::new(),
            anti_patterns: Vec::new(),
            source_refs: vec![crate::agent::reference_broker::SourceRef {
                url: "https://docs.rs/reqwest".to_string(),
                title: Some("reqwest docs".to_string()),
                accessed_at: Utc::now(),
                hash: None,
            }],
            confidence: ReferenceConfidence::Official,
            fetched_at: Some(Utc::now()),
        }];
        runtime.store.save_snapshot(&runtime.snapshot).unwrap();
        runtime.snapshot.repo_evidence.clear();
        runtime.snapshot.reference_packs.clear();

        let events = runtime.submit_input("/evidence").await.unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Modal { content, .. }
                if content.contains("Evidence Browser")
                    && content.contains("src/lib.rs:12")
                    && content.contains("reqwest")
                    && content.contains("https://docs.rs/reqwest")
        )));
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
    async fn out_of_scope_write_is_blocked_before_approval_queue() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/tui")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/core")).unwrap();
        std::fs::write(dir.path().join("src/tui/app.rs"), "fn tui() {}\n").unwrap();
        let model = fake_model(vec![
            Message {
                role: "assistant".to_string(),
                content: Some("Attempting edit".to_string()),
                tool_calls: Some(vec![ToolCallBlock {
                    id: "call-scope".to_string(),
                    r#type: "function".to_string(),
                    function: FunctionCall {
                        name: "write_file".to_string(),
                        arguments: serde_json::json!({
                            "file_path": "src/core/mod.rs",
                            "content": "pub fn outside() {}\n"
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
                content: Some("Stopped after scope guard".to_string()),
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
        runtime.set_autonomy(AutonomyLevel::Balanced, "test");

        let events = runtime
            .submit_input("Fix Mac shortcuts in the TUI")
            .await
            .unwrap();

        assert!(
            !events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::ApprovalRequested { .. }))
        );
        assert_eq!(runtime.snapshot().metadata.pending_approvals, 0);
        assert!(!dir.path().join("src/core/mod.rs").exists());
        assert!(events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolCallFinished { result, .. }
            if result.metadata
                .as_ref()
                .and_then(|meta| meta.get("blocked_by"))
                .and_then(Value::as_str)
                == Some("scope_guard")
        )));
    }

    #[tokio::test]
    async fn mutating_scheduler_blocks_later_mutations_after_policy_failure() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/tui")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/core")).unwrap();
        std::fs::write(dir.path().join("src/tui/app.rs"), "fn tui() {}\n").unwrap();
        let model = fake_model(vec![
            Message {
                role: "assistant".to_string(),
                content: Some("Inspecting then attempting edits".to_string()),
                tool_calls: Some(vec![
                    ToolCallBlock {
                        id: "call-read".to_string(),
                        r#type: "function".to_string(),
                        function: FunctionCall {
                            name: "read_range".to_string(),
                            arguments: serde_json::json!({
                                "file_path": "src/tui/app.rs",
                                "offset": 0,
                                "limit": 10
                            })
                            .to_string(),
                        },
                    },
                    ToolCallBlock {
                        id: "call-outside".to_string(),
                        r#type: "function".to_string(),
                        function: FunctionCall {
                            name: "write_file".to_string(),
                            arguments: serde_json::json!({
                                "file_path": "src/core/mod.rs",
                                "content": "pub fn outside() {}\n"
                            })
                            .to_string(),
                        },
                    },
                    ToolCallBlock {
                        id: "call-read-after".to_string(),
                        r#type: "function".to_string(),
                        function: FunctionCall {
                            name: "read_range".to_string(),
                            arguments: serde_json::json!({
                                "file_path": "src/tui/app.rs",
                                "offset": 0,
                                "limit": 10
                            })
                            .to_string(),
                        },
                    },
                    ToolCallBlock {
                        id: "call-inside".to_string(),
                        r#type: "function".to_string(),
                        function: FunctionCall {
                            name: "write_file".to_string(),
                            arguments: serde_json::json!({
                                "file_path": "src/tui/app.rs",
                                "content": "fn changed() {}\n"
                            })
                            .to_string(),
                        },
                    },
                ]),
                tool_call_id: None,
                reasoning: None,
                reasoning_details: None,
            },
            Message {
                role: "assistant".to_string(),
                content: Some("Stopped after mutation failure".to_string()),
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

        runtime
            .submit_input("Fix Mac shortcuts in the TUI")
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/tui/app.rs")).unwrap(),
            "fn tui() {}\n"
        );
        assert!(!dir.path().join("src/core/mod.rs").exists());
        let blocked_inside = runtime
            .snapshot()
            .messages
            .iter()
            .find(|message| message.tool_call_id.as_deref() == Some("call-inside"))
            .and_then(|message| message.content.as_deref())
            .and_then(|content| serde_json::from_str::<ToolResult>(content).ok())
            .expect("inside write result");
        assert!(!blocked_inside.success);
        assert_eq!(
            blocked_inside
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("blocked_by"))
                .and_then(Value::as_str),
            Some("mutating_scheduler")
        );
        let read_after = runtime
            .snapshot()
            .messages
            .iter()
            .find(|message| message.tool_call_id.as_deref() == Some("call-read-after"))
            .and_then(|message| message.content.as_deref())
            .and_then(|content| serde_json::from_str::<ToolResult>(content).ok())
            .expect("read after barrier result");
        assert!(read_after.success, "{read_after:?}");
    }

    #[test]
    fn mutation_barrier_detects_failed_and_running_mutating_tools() {
        let running = ToolResult {
            success: true,
            output: String::new(),
            error: None,
            metadata: Some(serde_json::json!({ "running": true })),
        };
        let stateful_command = ToolCall::RunCommand {
            command: "make generate".to_string(),
            cwd: None,
            blocking: false,
            timeout_ms: None,
            risk_class: RiskClass::StatefulExec,
        };
        assert!(
            mutation_barrier_reason(&stateful_command, "run_command", &running)
                .is_some_and(|reason| reason.contains("still running"))
        );

        let safe_command = ToolCall::RunCommand {
            command: "git status --short".to_string(),
            cwd: None,
            blocking: true,
            timeout_ms: None,
            risk_class: RiskClass::SafeExec,
        };
        assert!(mutation_barrier_reason(&safe_command, "run_command", &running).is_none());

        let failed_write = ToolResult {
            success: false,
            output: String::new(),
            error: Some("failed".to_string()),
            metadata: None,
        };
        assert!(
            mutation_barrier_reason(
                &ToolCall::WriteFile {
                    file_path: "src/lib.rs".to_string(),
                    content: String::new(),
                },
                "write_file",
                &failed_write,
            )
            .is_some_and(|reason| reason.contains("failed or was blocked"))
        );

        assert!(
            mutation_barrier_reason(
                &ToolCall::ReadRange {
                    file_path: "src/lib.rs".to_string(),
                    offset: None,
                    limit: None,
                },
                "read_range",
                &failed_write,
            )
            .is_none()
        );
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
            RuntimeEvent::Modal { content, .. } => content.contains("unused variable"),
            _ => false,
        }));

        let symbols = runtime.submit_input("/lsp symbols").await.unwrap();
        assert!(symbols.iter().any(|event| match event {
            RuntimeEvent::Modal { content, .. } => content.contains("run_session"),
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
    async fn runtime_soak_keeps_repl_state_across_streaming_approval_background_and_switch() {
        let dir = tempdir().unwrap();
        let dangerous = Message {
            role: "assistant".to_string(),
            content: Some("Need approval".to_string()),
            tool_calls: Some(vec![ToolCallBlock {
                id: "call-soak".to_string(),
                r#type: "function".to_string(),
                function: FunctionCall {
                    name: "run_command".to_string(),
                    arguments: serde_json::json!({
                        "command": "rm -rf /tmp/charm-soak",
                        "risk_class": "destructive"
                    })
                    .to_string(),
                },
            }]),
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        };
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
            fake_model(vec![dangerous]),
        )
        .await
        .unwrap();
        let first_session = runtime.snapshot().metadata.session_id.clone();

        let (tx, rx) = std::sync::mpsc::channel();
        runtime.submit_input_streaming("/help", tx).await.unwrap();
        let streaming_events = rx.try_iter().collect::<Vec<_>>();
        assert!(
            streaming_events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::StreamDone { .. }))
        );

        let approval_events = runtime.submit_input("dangerous cleanup").await.unwrap();
        let approval_id = approval_events
            .iter()
            .find_map(|event| match event {
                RuntimeEvent::ApprovalRequested { approval } => Some(approval.id.clone()),
                _ => None,
            })
            .expect("approval requested");
        assert_eq!(runtime.snapshot().metadata.pending_approvals, 1);

        let denial_events = runtime.resolve_approval(&approval_id, false).await.unwrap();
        assert!(denial_events.iter().any(|event| matches!(
            event,
            RuntimeEvent::ApprovalResolved { approval }
                if approval.status == ApprovalStatus::Denied
        )));
        assert_eq!(runtime.snapshot().metadata.pending_approvals, 0);

        runtime.subagent_bus.publish_for_session(
            &first_session,
            BackgroundJob {
                id: "soak-job".to_string(),
                title: "soak background".to_string(),
                status: BackgroundJobStatus::Completed,
                detail: "persisted from soak".to_string(),
                kind: BackgroundJobKind::SubAgent,
                progress: Some(100),
                metadata: None,
            },
        );
        let background_events = runtime.poll_background_events().unwrap();
        assert!(background_events.iter().any(|event| matches!(
            event,
            RuntimeEvent::BackgroundJobUpdated { job } if job.id == "soak-job"
        )));

        runtime.set_model("custom/soak".to_string());
        let mut unpinned = new_session_snapshot(dir.path(), Some("unpinned".to_string()));
        unpinned.metadata.session_id = "soak-unpinned".to_string();
        unpinned.metadata.pinned_model = None;
        runtime.store.save_snapshot(&unpinned).unwrap();
        runtime.switch_session_by_id("soak-unpinned").await.unwrap();

        assert_eq!(runtime.model_display(), "demo-model");
        assert!(runtime.snapshot().metadata.pinned_model.is_none());
        assert!(runtime.snapshot().approvals.is_empty());
        assert!(runtime.snapshot().background_jobs.is_empty());
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

    #[tokio::test]
    async fn agent_merge_copies_worktree_changes_to_primary_workspace() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let model = fake_model(Vec::new());
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

        let worktree = create_test_worktree(dir.path(), "merge-job");
        std::fs::write(worktree.join("subagent-output.txt"), "ready to merge").unwrap();
        runtime.snapshot.background_jobs.push(BackgroundJob {
            id: "merge-job-1234".to_string(),
            title: "merge test".to_string(),
            status: BackgroundJobStatus::Completed,
            detail: "ready".to_string(),
            kind: BackgroundJobKind::SubAgent,
            progress: Some(100),
            metadata: Some(serde_json::json!({
                "worktree_path": worktree,
                "changed_files": ["subagent-output.txt"],
                "turns": 2
            })),
        });

        let events = runtime
            .submit_input("/agent merge merge-job")
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("subagent-output.txt")).unwrap(),
            "ready to merge"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::MessageDelta { content, .. } if content.contains("Merged 1 file")
        )));
        let metadata = runtime.snapshot.background_jobs[0]
            .metadata
            .as_ref()
            .unwrap();
        assert_eq!(metadata.get("merged").and_then(Value::as_bool), Some(true));
    }

    #[tokio::test]
    async fn agent_export_writes_review_artifact_for_subagent_result() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let model = fake_model(Vec::new());
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

        let worktree = create_test_worktree(dir.path(), "export-job");
        std::fs::write(worktree.join("subagent-output.txt"), "ready to export").unwrap();
        runtime.snapshot.background_jobs.push(BackgroundJob {
            id: "export-job-1234".to_string(),
            title: "export test".to_string(),
            status: BackgroundJobStatus::Completed,
            detail: "ready".to_string(),
            kind: BackgroundJobKind::SubAgent,
            progress: Some(100),
            metadata: Some(serde_json::json!({
                "worktree_path": worktree,
                "changed_files": ["subagent-output.txt"],
                "turns": 2
            })),
        });

        let events = runtime
            .submit_input("/agent export export-job")
            .await
            .unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::MessageDelta { content, .. } if content.contains("Exported sub-agent")
        )));
        let metadata = runtime.snapshot.background_jobs[0]
            .metadata
            .as_ref()
            .unwrap();
        let export_path = metadata
            .get("export_path")
            .and_then(Value::as_str)
            .expect("export_path");
        let export = std::fs::read_to_string(export_path).unwrap();
        assert!(export.contains("export test"));
        assert!(export.contains("subagent-output.txt"));
        assert!(export.contains("ready to export"));
    }

    #[tokio::test]
    async fn agent_pr_writes_pull_request_draft_for_subagent_result() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let model = fake_model(Vec::new());
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

        let worktree = create_test_worktree(dir.path(), "pr-job");
        std::fs::write(worktree.join("subagent-output.txt"), "ready for pr").unwrap();
        runtime.snapshot.background_jobs.push(BackgroundJob {
            id: "pr-job-1234".to_string(),
            title: "pr draft test".to_string(),
            status: BackgroundJobStatus::Completed,
            detail: "implemented isolated change".to_string(),
            kind: BackgroundJobKind::SubAgent,
            progress: Some(100),
            metadata: Some(serde_json::json!({
                "worktree_path": worktree,
                "changed_files": ["subagent-output.txt"],
                "export_path": "/tmp/subagent-export.md",
                "turns": 2
            })),
        });

        let events = runtime.submit_input("/agent pr pr-job").await.unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::MessageDelta { content, .. } if content.contains("Wrote sub-agent PR draft")
        )));
        let metadata = runtime.snapshot.background_jobs[0]
            .metadata
            .as_ref()
            .unwrap();
        let draft_path = metadata
            .get("pr_draft_path")
            .and_then(Value::as_str)
            .expect("pr_draft_path");
        let draft = std::fs::read_to_string(draft_path).unwrap();
        assert!(draft.contains("pr draft test"));
        assert!(draft.contains("implemented isolated change"));
        assert!(draft.contains("subagent-output.txt"));
        assert!(draft.contains("/tmp/subagent-export.md"));
    }

    #[tokio::test]
    async fn agent_cleanup_removes_worktree_after_merge_review() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let model = fake_model(Vec::new());
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

        let worktree = create_test_worktree(dir.path(), "cleanup-job");
        runtime.snapshot.background_jobs.push(BackgroundJob {
            id: "cleanup-job-1234".to_string(),
            title: "cleanup test".to_string(),
            status: BackgroundJobStatus::Completed,
            detail: "ready".to_string(),
            kind: BackgroundJobKind::SubAgent,
            progress: Some(100),
            metadata: Some(serde_json::json!({
                "worktree_path": worktree,
                "changed_files": [],
                "merged": true
            })),
        });

        let events = runtime
            .submit_input("/agent cleanup cleanup-job")
            .await
            .unwrap();

        assert!(!worktree.exists());
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::MessageDelta { content, .. } if content.contains("Cleaned up")
        )));
        let metadata = runtime.snapshot.background_jobs[0]
            .metadata
            .as_ref()
            .unwrap();
        assert_eq!(
            metadata.get("cleaned_up").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[tokio::test]
    async fn agent_merge_rejects_changed_file_paths_that_escape_workspace() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let model = fake_model(Vec::new());
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

        let worktree = create_test_worktree(dir.path(), "escape-job");
        runtime.snapshot.background_jobs.push(BackgroundJob {
            id: "escape-job-1234".to_string(),
            title: "escape test".to_string(),
            status: BackgroundJobStatus::Completed,
            detail: "ready".to_string(),
            kind: BackgroundJobKind::SubAgent,
            progress: Some(100),
            metadata: Some(serde_json::json!({
                "worktree_path": worktree,
                "changed_files": ["../escape.txt"]
            })),
        });

        let err = runtime
            .submit_input("/agent merge escape-job")
            .await
            .unwrap_err();

        assert!(err.to_string().contains("escapes workspace"));
        assert!(!dir.path().join("escape.txt").exists());
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

    fn create_test_worktree(root: &Path, name: &str) -> PathBuf {
        let worktree = root.join(".charm").join("worktrees").join(name);
        std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        let output = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "--quiet",
                "--detach",
                &worktree.display().to_string(),
                "HEAD",
            ])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git worktree add failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        worktree
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
    async fn streaming_fallback_read_only_tools_run_as_parallel_batch() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("a.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(dir.path().join("src").join("b.rs"), "pub fn b() {}\n").unwrap();
        let fallback = Message {
            role: "assistant".to_string(),
            content: Some("Inspecting both files".to_string()),
            tool_calls: Some(vec![
                ToolCallBlock {
                    id: "call-stream-a".to_string(),
                    r#type: "function".to_string(),
                    function: FunctionCall {
                        name: "read_range".to_string(),
                        arguments: serde_json::json!({
                            "file_path": "src/a.rs",
                            "offset": 0,
                            "limit": 10
                        })
                        .to_string(),
                    },
                },
                ToolCallBlock {
                    id: "call-stream-b".to_string(),
                    r#type: "function".to_string(),
                    function: FunctionCall {
                        name: "read_range".to_string(),
                        arguments: serde_json::json!({
                            "file_path": "src/b.rs",
                            "offset": 0,
                            "limit": 10
                        })
                        .to_string(),
                    },
                },
            ]),
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        };

        let (mut runtime, _) = SessionRuntime::bootstrap(
            dir.path(),
            "demo-model".to_string(),
            "openrouter".to_string(),
            InteractiveRequest {
                prompt: Some("Test fallback batch".to_string()),
                new_session: true,
                continue_last: false,
                session_id: None,
            },
            fake_model_no_stream(fallback),
        )
        .await
        .unwrap();
        let session_id = runtime.snapshot().metadata.session_id.clone();

        let (tx, rx) = std::sync::mpsc::channel();
        runtime
            .submit_input_streaming("Read both files", tx)
            .await
            .unwrap();

        let events: Vec<RuntimeEvent> = rx.try_iter().collect();
        let tool_event_order = events
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::ToolCallStarted { .. } => Some("started"),
                RuntimeEvent::ToolCallFinished { .. } => Some("finished"),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            tool_event_order,
            vec!["started", "started", "finished", "finished"]
        );
        let tool_call_ids = runtime
            .snapshot()
            .messages
            .iter()
            .filter(|message| message.role == "tool")
            .filter_map(|message| message.tool_call_id.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(tool_call_ids, vec!["call-stream-a", "call-stream-b"]);

        let trace_path = dir
            .path()
            .join(".charm")
            .join("traces")
            .join(format!("{session_id}.jsonl"));
        let trace = std::fs::read_to_string(trace_path).expect("trace jsonl");
        assert!(trace.contains("\"event\":\"parallel_tool_batch\""));
    }

    #[tokio::test]
    async fn streaming_internal_command_emits_done_event() {
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

        let (tx, rx) = std::sync::mpsc::channel();
        runtime.submit_input_streaming("/help", tx).await.unwrap();

        let events: Vec<RuntimeEvent> = rx.try_iter().collect();
        assert!(
            events.iter().any(
                |event| matches!(event, RuntimeEvent::Modal { content, .. } if content.contains("Charm Help"))
            )
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::StreamDone { .. })),
            "internal commands must clear TUI processing state"
        );
    }

    #[tokio::test]
    async fn streaming_empty_intent_override_emits_done_event() {
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

        let (tx, rx) = std::sync::mpsc::channel();
        runtime.submit_input_streaming("/plan", tx).await.unwrap();

        let events: Vec<RuntimeEvent> = rx.try_iter().collect();
        assert!(
            events.iter().any(
                |event| matches!(event, RuntimeEvent::MessageDelta { content, .. } if content.contains("Intent set to Plan"))
            )
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::StreamDone { .. })),
            "empty slash intent overrides must clear TUI processing state"
        );
    }

    #[tokio::test]
    async fn streaming_model_failure_reports_error_without_ending_repl() {
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
            Arc::new(FailingModel),
        )
        .await
        .unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        runtime.submit_input_streaming("hello", tx).await.unwrap();

        let events: Vec<RuntimeEvent> = rx.try_iter().collect();
        assert!(
            events.iter().any(
                |event| matches!(event, RuntimeEvent::MessageDelta { role, content } if role == "system" && content.contains("Model error"))
            )
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::StreamDone { .. }))
        );
    }

    #[tokio::test]
    async fn streaming_fallback_approval_request_emits_done_event() {
        let dir = tempdir().unwrap();
        let model_message = Message {
            role: "assistant".to_string(),
            content: Some("Need approval".to_string()),
            tool_calls: Some(vec![ToolCallBlock {
                id: "call-approval".to_string(),
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
        };
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
            fake_model_no_stream(model_message),
        )
        .await
        .unwrap();
        runtime.set_autonomy(AutonomyLevel::Conservative, "test");

        let (tx, rx) = std::sync::mpsc::channel();
        runtime.submit_input_streaming("danger", tx).await.unwrap();

        let events: Vec<RuntimeEvent> = rx.try_iter().collect();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::ApprovalRequested { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::StreamDone { .. })),
            "approval requests must clear TUI processing state"
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
