use crate::agent::provider_prompts::ProviderPrompts;
use crate::agent::rules::RulesLoader;
use crate::core::{ContextStack, WorkspaceState};
use crate::harness::{MemoryManager, PlanManager};
use crate::runtime::types::{
    AutonomyLevel, LspSnapshot, McpSnapshot, RouterIntent, WorkspacePreflight,
};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum AgentMode {
    Build,
    Plan,
    Explore,
}

pub struct PromptAssembler {
    plan_manager: PlanManager,
    workspace_root: std::path::PathBuf,
    mode: AgentMode,
    provider_hint: String,
}

impl PromptAssembler {
    pub fn new(workspace_root: &Path) -> Self {
        Self {
            plan_manager: PlanManager::new(workspace_root),
            workspace_root: workspace_root.to_path_buf(),
            mode: AgentMode::Build,
            provider_hint: "default".to_string(),
        }
    }

    pub fn with_mode(mut self, mode: AgentMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_provider(mut self, provider: &str) -> Self {
        self.provider_hint = provider.to_string();
        self
    }

    pub fn assemble(
        &self,
        workspace: &WorkspaceState,
        user_message: &str,
        evidence: Vec<String>,
    ) -> String {
        let plan = self.plan_manager.load();
        let mut parts = Vec::new();

        parts.push(system_identity());
        parts.push(communication_style());
        parts.push(provider_specific_rules(&self.provider_hint));
        parts.push(tool_calling_rules());
        parts.push(making_code_changes_rules());
        parts.push(debugging_rules());
        parts.push(memory_rules());
        parts.push(mode_rules(&self.mode));

        let rules = RulesLoader::load_all(&self.workspace_root);
        if !rules.is_empty() {
            parts.push(format!("## Rules\n{}\n", rules.join("\n\n---\n\n")));
        }

        let mem_manager = MemoryManager::new(&self.workspace_root);
        let approved = mem_manager.store().get_approved(Some("project"));
        if !approved.is_empty() {
            let mem_text: Vec<String> = approved
                .iter()
                .map(|e| format!("- [{}] {}", e.category, e.content))
                .collect();
            parts.push(format!("## Memories\n{}\n", mem_text.join("\n")));
        }

        parts.push(format!(
            "## Workspace\n- Root: {}\n- Branch: {}\n- Dirty: {}\n",
            workspace.root_path,
            workspace.branch,
            if workspace.dirty_files.is_empty() {
                "none".to_string()
            } else {
                workspace.dirty_files.join(", ")
            }
        ));

        parts.push(format!(
            "## Plan\n- Objective: {}\n- Phase: {}\n- Done: {}\n- Blocked: {}\n",
            plan.objective,
            plan.current_phase,
            plan.completed_steps.join(", "),
            plan.blocked_steps.join(", ")
        ));

        if !evidence.is_empty() {
            parts.push(format!("## Evidence\n{}\n", evidence.join("\n\n---\n\n")));
        }

        parts.push(format!("## Task\n{}\n", user_message));
        parts.push("Narrate your reasoning, then place tool calls at the end.".to_string());

        parts.join("\n\n")
    }

    pub fn assemble_session_system(
        &self,
        workspace: &WorkspaceState,
        intent: RouterIntent,
        autonomy: AutonomyLevel,
        preflight: &WorkspacePreflight,
        lsp: &LspSnapshot,
        mcp: &McpSnapshot,
    ) -> String {
        let mut parts = Vec::new();

        parts.push(system_identity());
        parts.push(communication_style());
        parts.push(provider_specific_rules(&self.provider_hint));
        parts.push(tool_calling_rules());
        parts.push(making_code_changes_rules());
        parts.push(debugging_rules());
        parts.push(memory_rules());
        parts.push(interactive_workflow_rules(intent, autonomy));

        let rules = RulesLoader::load_all(&self.workspace_root);
        if !rules.is_empty() {
            parts.push(format!("## Rules\n{}\n", rules.join("\n\n---\n\n")));
        }

        parts.push(format!(
            "## Workspace\n- Root: {}\n- Branch: {}\n- Dirty: {}\n",
            workspace.root_path,
            workspace.branch,
            if workspace.dirty_files.is_empty() {
                "none".to_string()
            } else {
                workspace.dirty_files.join(", ")
            }
        ));

        parts.push(format!(
            "## Preflight\n- Recent: {}\n- Suggested: {}\n",
            preflight
                .recent_summary
                .clone()
                .unwrap_or_else(|| "none".to_string()),
            if preflight.suggested_actions.is_empty() {
                "none".to_string()
            } else {
                preflight.suggested_actions.join(" | ")
            }
        ));

        parts.push(format!(
            "## LSP\n- Ready: {}\n- Roots: {}\n- Diagnostics: {}\n- Symbols: {}\n- Servers: {}\n- Jumps: {}\n",
            lsp.ready,
            if lsp.active_roots.is_empty() {
                "none".to_string()
            } else {
                lsp.active_roots.join(", ")
            },
            lsp.diagnostics.len(),
            lsp.symbol_provider,
            if lsp.servers.is_empty() {
                "none".to_string()
            } else {
                lsp.servers
                    .iter()
                    .map(|server| format!("{}:{}:{}", server.language, server.command, server.ready))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            if lsp.symbol_jumps.is_empty() {
                "none".to_string()
            } else {
                lsp.symbol_jumps
                    .iter()
                    .take(5)
                    .map(|jump| format!("{}@{}:{}", jump.name, jump.file_path, jump.line))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ));

        parts.push(format!(
            "## MCP\n- Ready: {}\n- Servers: {}\n- Tools: {}\n",
            mcp.ready,
            if mcp.servers.is_empty() {
                "none".to_string()
            } else {
                mcp.servers
                    .iter()
                    .map(|server| {
                        format!(
                            "{}:{}({})",
                            server.name, server.tool_count, server.approval_mode
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            if mcp.tools.is_empty() {
                "none".to_string()
            } else {
                mcp.tools.join(", ")
            }
        ));

        parts.join("\n\n")
    }
}

fn system_identity() -> String {
    "You are Charm, an autonomous coding agent. Your goal: understand the codebase, plan changes, execute correctly, and verify results.".to_string()
}

fn tool_calling_rules() -> String {
    r#"## Tool Calling
- Only call tools when absolutely necessary. If you already know the answer, respond without tools.
- If you state you will use a tool, immediately call that tool as your next action.
- Follow the tool call schema exactly and provide all required parameters.
- Never call tools not explicitly provided in your system prompt.
- Before calling each tool, first explain why you are calling it.
- Some tools run asynchronously. If you need to see output before continuing, stop making new tool calls.
- Place ALL tool calls at the END of your message.
- Parallelize independent read-only calls."#
        .to_string()
}

fn making_code_changes_rules() -> String {
    r#"## Code Changes
- NEVER output code to the user unless requested. Use tools to implement changes.
- Your generated code must be immediately runnable. Add all necessary imports and dependencies.
- Read the full file before editing to understand context.
- Match existing code style exactly (indentation, naming, patterns).
- For edits: provide exact old_string including leading/trailing whitespace.
- Combine ALL changes to a SINGLE file into ONE edit_patch call.
- If a file is large, read the relevant section first, then edit.
- After editing, verify with run_command when tests are available.
- Create checkpoints before risky operations.
- When running commands: NEVER include `cd` in the command string. Use the tool's cwd parameter."#
        .to_string()
}

fn debugging_rules() -> String {
    r#"## Debugging
- Focus on root cause, not symptoms.
- Reproduce the issue before fixing.
- Check related files: callers, tests, type definitions.
- Use grep_search to find where symbols are defined and used.
- Run targeted tests after each fix attempt.
- If stuck, re-read the problem statement and re-examine assumptions."#
        .to_string()
}

fn communication_style() -> String {
    r#"## Communication
- BE CONCISE AND AVOID VERBOSITY. BREVITY IS CRITICAL.
- Minimize output tokens while maintaining accuracy.
- Only address the specific query or task at hand.
- Format responses in markdown. Use backticks for file/function names.
- Refer to the user in second person, yourself in first person."#
        .to_string()
}

fn memory_rules() -> String {
    r#"## Memory
- When you encounter important information (patterns, preferences, architecture), stage it with memory_stage.
- You do NOT need permission to create memories.
- Relevant memories are automatically retrieved and presented to you."#
        .to_string()
}

fn provider_specific_rules(provider_hint: &str) -> String {
    let provider = ProviderPrompts::resolve_provider(provider_hint);
    let prompts = ProviderPrompts::new();
    let specific = prompts.get(provider);
    if specific.is_empty() {
        String::new()
    } else {
        format!("## Provider Notes\n{}\n", specific)
    }
}

fn mode_rules(mode: &AgentMode) -> String {
    match mode {
        AgentMode::Plan => "## Mode: Plan\nYou are in planning mode. Analyze the codebase, understand the problem, and produce a detailed implementation plan. Do NOT make code changes yet. Use read and search tools only.".to_string(),
        AgentMode::Explore => "## Mode: Explore\nYou are in exploration mode. Read code, understand patterns, and report findings. Do NOT make changes. Use read and search tools only.".to_string(),
        AgentMode::Build => "## Mode: Build\nYou are in build mode. Execute the plan, make code changes, run tests, and verify.".to_string(),
    }
}

fn interactive_workflow_rules(intent: RouterIntent, autonomy: AutonomyLevel) -> String {
    format!(
        "## Interactive Session\n- You are operating in a multi-turn coding session.\n- Current intent: {intent:?}.\n- Default workflow: understand -> plan -> execute -> verify -> summarize.\n- Slash commands may override intent for the current turn only.\n- Autonomy level: {autonomy:?}.\n- Under aggressive autonomy, reads/searches/edits/tests are automatic; destructive or external side effects require approval.\n- Use LSP and MCP context when relevant.\n- Keep the conversation grounded in the current workspace state."
    )
}
