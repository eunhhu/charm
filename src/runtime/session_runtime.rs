use super::mcp::discover_mcp_tools;
use super::router::{decide_intent, parse_slash_override, requires_approval};
use super::types::{
    ApprovalRequest, ApprovalStatus, AutonomyLevel, LspSnapshot, McpSnapshot, RouterIntent,
    RuntimeEvent, SessionLifecycle, ToolExecution, WorkspacePreflight,
};
use super::workspace::{build_preflight, collect_lsp_snapshot};
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
use crate::providers::types::{ChatRequest, Message, ToolSchema, Usage};
use crate::tools::ToolRegistry;
use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

#[async_trait]
pub trait RuntimeModel: Send + Sync {
    async fn chat(&self, request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)>;
    fn tool_schemas(&self) -> Vec<ToolSchema>;
}

#[async_trait]
impl RuntimeModel for ProviderClient {
    async fn chat(&self, request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)> {
        ProviderClient::chat(self, request).await
    }

    fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.build_tool_schemas()
    }
}

pub struct SessionRuntime {
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
                content: format!(
                    "MCP servers: {} | tools: {}",
                    self.mcp
                        .servers
                        .iter()
                        .map(|server| server.name.clone())
                        .collect::<Vec<_>>()
                        .join(", "),
                    self.mcp.tools.join(", ")
                ),
            }])),
            ["/lsp"] => Ok(Some(vec![RuntimeEvent::MessageDelta {
                role: "assistant".to_string(),
                content: format!(
                    "LSP ready={} roots={} diagnostics={}",
                    self.lsp.ready,
                    self.lsp.active_roots.join(", "),
                    self.lsp.diagnostics.len()
                ),
            }])),
            _ => Ok(None),
        }
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
    use crate::providers::types::{FunctionCall, ToolCallBlock};
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
}
