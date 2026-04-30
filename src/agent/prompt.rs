use crate::agent::prompt_compiler::{
    Activation, PromptCompiler, PromptContext, PromptSection, ProviderHint, SectionType,
};
use crate::agent::provider_prompts::ProviderPrompts;
use crate::agent::reference_broker::ReferencePack;
use crate::agent::rules::RulesLoader;
use crate::agent::task_concretizer::TaskContract;
use crate::core::WorkspaceState;
use crate::harness::{MemoryManager, PlanManager};
use crate::retrieval::types::Evidence;
use crate::runtime::types::{
    AutonomyLevel, LspSnapshot, McpSnapshot, RouterIntent, VerificationState, WorkspacePreflight,
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

pub struct SessionPromptContext<'a> {
    pub workspace: &'a WorkspaceState,
    pub intent: RouterIntent,
    pub autonomy: AutonomyLevel,
    pub preflight: &'a WorkspacePreflight,
    pub lsp: &'a LspSnapshot,
    pub mcp: &'a McpSnapshot,
    pub task_contract: Option<&'a TaskContract>,
    pub verification: &'a VerificationState,
    pub repo_evidence: &'a [Evidence],
    pub reference_packs: &'a [ReferencePack],
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
        let mut parts = vec![
            system_identity(),
            communication_style(),
            provider_specific_rules(&self.provider_hint),
            tool_calling_rules(),
            making_code_changes_rules(),
            debugging_rules(),
            memory_rules(),
            mode_rules(&self.mode),
        ];

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

    pub fn assemble_session_system(&self, context: SessionPromptContext<'_>) -> String {
        let mut parts = vec![
            system_identity(),
            communication_style(),
            provider_specific_rules(&self.provider_hint),
            tool_calling_rules(),
            making_code_changes_rules(),
            debugging_rules(),
            memory_rules(),
            interactive_workflow_rules(context.intent, context.autonomy),
        ];

        let rules = RulesLoader::load_all(&self.workspace_root);
        if !rules.is_empty() {
            parts.push(format!("## Rules\n{}\n", rules.join("\n\n---\n\n")));
        }

        parts.push(format!(
            "## Workspace\n- Root: {}\n- Branch: {}\n- Dirty: {}\n",
            context.workspace.root_path,
            context.workspace.branch,
            if context.workspace.dirty_files.is_empty() {
                "none".to_string()
            } else {
                context.workspace.dirty_files.join(", ")
            }
        ));

        parts.push(format!(
            "## Preflight\n- Recent: {}\n- Suggested: {}\n",
            context
                .preflight
                .recent_summary
                .clone()
                .unwrap_or_else(|| "none".to_string()),
            if context.preflight.suggested_actions.is_empty() {
                "none".to_string()
            } else {
                context.preflight.suggested_actions.join(" | ")
            }
        ));

        parts.push(format!(
            "## LSP\n- Ready: {}\n- Roots: {}\n- Diagnostics: {}\n- Symbols: {}\n- Servers: {}\n- Jumps: {}\n",
            context.lsp.ready,
            if context.lsp.active_roots.is_empty() {
                "none".to_string()
            } else {
                context.lsp.active_roots.join(", ")
            },
            context.lsp.diagnostics.len(),
            context.lsp.symbol_provider,
            if context.lsp.servers.is_empty() {
                "none".to_string()
            } else {
                context
                    .lsp
                    .servers
                    .iter()
                    .map(|server| format!("{}:{}:{}", server.language, server.command, server.ready))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            if context.lsp.symbol_jumps.is_empty() {
                "none".to_string()
            } else {
                context
                    .lsp
                    .symbol_jumps
                    .iter()
                    .take(5)
                    .map(|jump| format!("{}@{}:{}", jump.name, jump.file_path, jump.line))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ));

        parts.push(format!(
            "## MCP\n- Ready: {}\n- Servers: {}\n- Tools: {}\n",
            context.mcp.ready,
            if context.mcp.servers.is_empty() {
                "none".to_string()
            } else {
                context
                    .mcp
                    .servers
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
            if context.mcp.tools.is_empty() {
                "none".to_string()
            } else {
                context.mcp.tools.join(", ")
            }
        ));

        parts.push(compiled_harness_rules(
            context.task_contract,
            context.verification,
            context.repo_evidence,
            context.reference_packs,
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
    let autonomy_note = match autonomy {
        AutonomyLevel::Conservative => {
            "You must request approval for any write/exec/external tool. Prefer reads, searches, and summaries."
        }
        AutonomyLevel::Balanced => {
            "Reads and safe execution are automatic. Stateful edits, destructive ops, and external side effects require approval."
        }
        AutonomyLevel::Aggressive => {
            "Reads, searches, edits, and tests run automatically. Only destructive or external-side-effect work escalates for approval."
        }
        AutonomyLevel::Yolo => {
            "YOLO MODE: every tool is auto-approved including destructive and external-side-effect operations. A loud ⚠ transcript entry is still emitted for destructive/external calls (trace requirement). Favor decisive action, but: (1) create a git stash or checkpoint before irreversible operations, (2) prefer tests before destructive runs, (3) treat external API calls as something the user will have to audit after the fact. The user chose YOLO to accept responsibility, not to skip evidence — keep trace-first discipline."
        }
    };
    format!(
        "## Interactive Session\n- You are operating in a multi-turn coding session.\n- Current intent: {intent:?}. You may self-select the intent you need next turn; the router will keep up.\n- Default workflow: understand -> plan -> execute -> verify -> summarize.\n- Slash commands override intent for the current turn only (/explore /plan /build /verify).\n- Autonomy level: {label} ({short}). {autonomy_note}\n- Use LSP and MCP context when relevant. Never fabricate APIs; verify with read/search tools.\n- Keep the conversation grounded in the current workspace state.\n- You can request long-running work via background sub-agents when the user mentions \"background\" or \"in parallel\".",
        label = autonomy.label(),
        short = autonomy.short(),
    )
}

fn task_contract_rules(contract: &TaskContract) -> String {
    format!(
        "## Current Task Contract\n- Objective: {}\n- Abstraction score: {:.2}\n- Depth: {:?}\n- Scope: {}\n- Acceptance: {}\n- Verification: {}\n- Side effects: {}\n- Assumptions: {}\n- Open questions: {}\n",
        contract.objective,
        contract.abstraction_score,
        contract.depth,
        join_or_none(&contract.scope),
        join_or_none(&contract.acceptance),
        join_or_none(&contract.verification),
        join_or_none(&contract.side_effects),
        join_or_none(&contract.assumptions),
        join_or_none(&contract.open_questions),
    )
}

fn verification_gate_rules(verification: &VerificationState) -> String {
    format!(
        "## Verification Gate\n- Required evidence: {}\n- Observed evidence: {}\n- Satisfied: {}\n- Last status: {}\n- Do not claim completion unless this gate is satisfied or you explicitly state the verification gap.\n",
        join_or_none(&verification.required),
        join_or_none(&verification.observed),
        verification.satisfied,
        verification
            .last_status
            .clone()
            .unwrap_or_else(|| "none".to_string()),
    )
}

fn compiled_harness_rules(
    task_contract: Option<&TaskContract>,
    verification: &VerificationState,
    repo_evidence: &[Evidence],
    reference_packs: &[ReferencePack],
) -> String {
    let mut compiler = PromptCompiler::new().with_budget(2600);
    if let Some(contract) = task_contract {
        compiler.add_section(PromptSection {
            id: "current_task_contract".to_string(),
            priority: 10,
            activation: Activation::Always,
            token_budget: 900,
            content: task_contract_rules(contract),
            provenance: Vec::new(),
            section_type: SectionType::Plan,
        });
    }
    compiler.add_section(PromptSection {
        id: "verification_gate".to_string(),
        priority: 20,
        activation: Activation::Always,
        token_budget: 500,
        content: verification_gate_rules(verification),
        provenance: Vec::new(),
        section_type: SectionType::Evidence,
    });
    if !repo_evidence.is_empty() {
        compiler.add_section(PromptSection {
            id: "repo_evidence".to_string(),
            priority: 30,
            activation: Activation::Always,
            token_budget: 800,
            content: repo_evidence_rules(repo_evidence),
            provenance: Vec::new(),
            section_type: SectionType::Evidence,
        });
    }
    if !reference_packs.is_empty() {
        compiler.add_section(PromptSection {
            id: "reference_gate".to_string(),
            priority: 40,
            activation: Activation::Always,
            token_budget: 800,
            content: reference_gate_rules(reference_packs),
            provenance: Vec::new(),
            section_type: SectionType::Reference,
        });
    }

    let context = PromptContext {
        token_budget_remaining: 2600,
        ..Default::default()
    };
    let compiled = compiler.compile(&context);
    compiler.render_for_provider(&compiled, ProviderHint::Generic)
}

fn repo_evidence_rules(evidence: &[Evidence]) -> String {
    let rows = evidence
        .iter()
        .take(8)
        .map(|item| {
            format!(
                "- [{} {:.1}] {}:{} {}",
                item.source,
                item.rank,
                item.file_path,
                item.line,
                item.snippet.lines().next().unwrap_or("").trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "## Repo Evidence\n{}\nUse these file/line anchors before making or judging code changes.",
        rows
    )
}

fn reference_gate_rules(reference_packs: &[ReferencePack]) -> String {
    let rows = reference_packs
        .iter()
        .take(5)
        .map(|pack| {
            let examples = pack
                .minimal_examples
                .iter()
                .take(2)
                .map(|example| compact_reference_example(&example.code))
                .collect::<Vec<_>>();
            format!(
                "- {} confidence={:?} source={:?} examples={} caveats={}",
                pack.library.clone().unwrap_or_else(|| pack.query.clone()),
                pack.confidence,
                pack.source_kind,
                if examples.is_empty() {
                    "none".to_string()
                } else {
                    examples.join(" | ")
                },
                if pack.caveats.is_empty() {
                    "none".to_string()
                } else {
                    pack.caveats.join(" | ")
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "## Reference Gate\n{}\nDo not rely on model memory alone for external APIs. Use package/source/docs evidence before implementation claims.",
        rows
    )
}

fn compact_reference_example(raw: &str) -> String {
    let text = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() <= 180 {
        return text;
    }
    let mut out = text.chars().take(180).collect::<String>();
    out.push_str("...");
    out
}

fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(" | ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::task_concretizer::{ExecutionDepth, RepoAnchor, TaskContract};
    use crate::core::WorkspaceState;
    use crate::runtime::types::{AutonomyLevel, RouterIntent};

    #[test]
    fn session_system_prompt_budgets_harness_contract_sections() {
        let assembler = PromptAssembler::new(std::path::Path::new("."));
        let workspace = WorkspaceState {
            root_path: ".".to_string(),
            branch: "main".to_string(),
            dirty_files: Vec::new(),
            open_files: Vec::new(),
        };
        let contract = TaskContract {
            abstraction_score: 0.8,
            objective: "x".repeat(12_000),
            scope: vec!["all".to_string()],
            repo_anchors: Vec::<RepoAnchor>::new(),
            acceptance: vec!["done".to_string()],
            verification: vec!["check".to_string()],
            side_effects: Vec::new(),
            assumptions: Vec::new(),
            open_questions: Vec::new(),
            depth: ExecutionDepth::Deep,
        };

        let prompt = assembler.assemble_session_system(SessionPromptContext {
            workspace: &workspace,
            intent: RouterIntent::Implement,
            autonomy: AutonomyLevel::Aggressive,
            preflight: &WorkspacePreflight::default(),
            lsp: &LspSnapshot::default(),
            mcp: &McpSnapshot::default(),
            task_contract: Some(&contract),
            verification: &VerificationState::default(),
            repo_evidence: &[],
            reference_packs: &[],
        });

        assert!(prompt.contains("## Current Task Contract"));
        assert!(prompt.contains("characters omitted"));
        assert!(prompt.len() < 9_000);
    }
}
