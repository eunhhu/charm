# Devin/Windsurf Harness Research

Date: 2026-04-23

## Scope

This document captures public-source research and product-level analysis for:

- Cognition Devin
- Windsurf Cascade
- The combined Devin-in-Windsurf direction
- Design implications for Charm

This is not a reconstruction of private system prompts. Do not use leaked prompts, credentialed internal material, or prompt-extraction techniques. The useful target is the harness: context assembly, retrieval, tool policy, execution loop, delegation, and session memory.

## Executive Summary

The visible advantage of Devin and Windsurf is not a single magic prompt. It is a full agent harness that compiles the right prompt every turn from workspace state, durable knowledge, rules, retrieved evidence, plan state, tool policy, and execution history.

Charm should therefore optimize around:

1. A typed context compiler instead of ad hoc prompt string concatenation.
2. A fast retrieval worker that gathers precise evidence before the main model reasons.
3. Rules, skills, workflows, and memories as separate primitives.
4. A trace store that connects user intent, tool calls, edits, tests, and outcomes.
5. Delegation primitives that can run local or remote child agents with isolated state.

## Product Model Comparison

| Dimension | Windsurf Cascade | Devin |
| --- | --- | --- |
| Primary environment | Local IDE | Cloud VM / remote agent session |
| Best at | Interactive coding, local context, fast edit loop | Async engineering tasks, PRs, testing, long-running execution |
| Context sources | Open files, selected text, terminal state, diagnostics, rules, memories, codebase index | Repo context, knowledge, playbooks, session history, Ask Devin, DeepWiki-style repo docs |
| User controls | Chat/code modes, rules, memories, skills, workflows, checkpoints | Knowledge, playbooks, scheduled work, managed Devins, API/MCP |
| Delegation model | Multiple Cascade sessions, IDE-local flow | Parent Devin can manage child Devins and independent sessions |
| Persistence | IDE workspace config plus memories | Stateful cloud sessions, durable knowledge, scheduled sessions |

The likely integration direction is local/cloud split:

- Windsurf remains the fast local IDE control surface.
- Devin handles async implementation, QA, PRs, and multi-agent delegation.
- The useful interface between them is task handoff plus shared repo/context artifacts.

## Inferred Prompt Stack

Public documentation implies a layered prompt compiler roughly like this:

```text
System identity
Mode policy: chat / code / plan / verify
Workspace snapshot: branch, dirty files, open files, selected text, terminal, diagnostics
User rules: global rules, workspace rules, AGENTS.md-style instructions, activation mode
Knowledge and memory: repo conventions, durable memories, user preferences
Task context: current prompt, issue, PR, attached files, selected code
Retrieval pack: relevant files, line ranges, codemap/wiki snippets, prior traces
Plan state: objective, todo list, current phase, blockers
Tool policy: available tools, risk class, approval rules, budgets
Execution contract: edit style, test policy, report format
Reflection hook: summarize, store memory/trace, suggest workflow/playbook updates
```

Charm currently has early versions of several layers:

- `PromptAssembler` for system prompt assembly.
- `SessionRuntime` for workspace state, LSP snapshot, MCP snapshot, routing, and approvals.
- `ToolRegistry` for local tools.
- `PlanManager` and `MemoryManager` for plan and memory artifacts.
- `SessionStore` for persisted sessions.

The missing piece is a typed compiler that owns priorities, token budgets, activation conditions, and evidence provenance.

## Harness Loop

A competitive harness should run this loop:

```text
1. Ingest user input
2. Classify intent: explore / plan / implement / verify
3. Build workspace snapshot
4. Activate rules, skills, workflows, memories
5. Run fast context retrieval if task needs code evidence
6. Compile prompt layers under token budget
7. Call model
8. Execute tool calls with risk policy and checkpoints
9. Update plan, trace, session, and memory
10. Verify with tests/checks when behavior changed
11. Summarize outcome and next action
```

Key point: retrieval should not be left entirely to the main model. A small retrieval worker can find evidence faster and cheaper, then pass a compact evidence pack into the main reasoning turn.

## Context System Design

### Fast Context Worker

Purpose: gather task-relevant code evidence before the main model acts.

Properties:

- Tool-limited: `glob`, `grep`, `read_range`, `semantic_search`, optionally `lsp_symbols`.
- No writes, no shell mutation.
- Bounded turns and bounded output.
- Returns file paths, line ranges, symbols, and confidence.
- Does not summarize code into vague prose when exact references are available.

Output shape:

```json
{
  "query": "string",
  "evidence": [
    {
      "path": "src/runtime/session_runtime.rs",
      "line_start": 174,
      "line_end": 223,
      "kind": "function",
      "symbol": "SessionRuntime::submit_input",
      "reason": "handles user input routing and model loop",
      "confidence": 0.86
    }
  ],
  "misses": ["areas searched but not found"],
  "recommended_next_reads": []
}
```

### Codemap

Purpose: task-specific dependency map.

Codemap should be generated from:

- Symbol index.
- Imports/call edges.
- Recently touched files.
- Test ownership.
- LSP symbol jumps.
- Search evidence.

Codemap is not a wiki. It is narrow and task-scoped.

### DeepWiki-Style Repo Knowledge

Purpose: persistent repo-level explanation.

Wiki artifacts should describe:

- Architecture boundaries.
- Main execution flows.
- Provider contracts.
- Tool registry contracts.
- Session persistence format.
- Testing strategy.

Wiki is updated deliberately, not every turn.

## Customization Primitives

Keep these separate. Merging them creates ambiguous prompt behavior.

### Rules

Rules are behavioral constraints.

Examples:

- Always run `rtk cargo test` after Rust behavior changes.
- Never edit generated files.
- Use provider-specific auth naming rules.

Recommended activation modes:

- `always_on`
- `glob`
- `manual`
- `model_decision`

### Skills

Skills are procedural capability packs.

A skill can contain:

- Metadata.
- Trigger rules.
- Instructions.
- Helper scripts.
- Templates/assets.

Only metadata should be loaded by default. Full skill body should load only when invoked.

### Workflows

Workflows are user-invoked runbooks.

Examples:

- `/release-check`
- `/fix-ci`
- `/review-pr`
- `/write-spec`

Workflows should be deterministic step lists with optional model calls, not passive prompt rules.

### Memories

Memories are learned facts.

Recommended scopes:

- `session`: short-lived, auto-approved.
- `project`: durable repo facts, reviewable.
- `user`: user preferences, reviewable.

Memories must be evidence-backed where possible.

## Delegation Model

Devin's public direction emphasizes managed child agents and scheduled work. Charm can mirror this locally first.

### Parent Session

Responsibilities:

- Decompose task.
- Assign write scopes.
- Start child sessions.
- Track progress.
- Merge results.
- Run final verification.

### Child Session

Properties:

- Narrow task.
- Isolated write scope.
- No global refactor.
- Reports changed files, tests run, blockers, and confidence.

### Isolation Levels

1. Same worktree, read-only exploration.
2. Same worktree, disjoint write sets.
3. Separate git worktree.
4. Remote runner or cloud VM.

Charm should support levels 1-3 before remote execution.

## Agent Trace

An agent trace links:

- User request.
- Retrieval evidence.
- Prompt layers used.
- Model response.
- Tool calls.
- File edits.
- Commands/tests.
- Result status.
- Follow-up memories.

Trace enables:

- "Why did this code change?"
- "Which test proved this?"
- "Which context did the agent miss?"
- "What should become a rule/workflow?"

Minimal trace schema:

```json
{
  "session_id": "uuid",
  "turn_id": "uuid",
  "intent": "implement",
  "evidence_ids": ["uuid"],
  "tool_calls": ["uuid"],
  "edits": [
    {
      "path": "src/tools/command.rs",
      "line_start": 1,
      "line_end": 80,
      "summary": "capture stdout/stderr for background commands"
    }
  ],
  "commands": [
    {
      "command": "rtk cargo test command",
      "exit_code": 0
    }
  ],
  "outcome": "passed"
}
```

## Charm Implementation Roadmap

### Phase 1: Prompt Compiler

Replace ad hoc prompt assembly with typed sections:

```rust
pub struct PromptSection {
    pub id: String,
    pub priority: u8,
    pub activation: Activation,
    pub token_budget: usize,
    pub content: String,
    pub provenance: Vec<SourceRef>,
}
```

Required behavior:

- Stable section order.
- Token budget enforcement.
- Source provenance.
- Provider-specific rendering.
- Test snapshots for compiled prompts.

### Phase 2: Fast Context Worker

Add a worker that returns structured evidence from search/index/LSP.

Required behavior:

- Read-only.
- Bounded time and turns.
- Dedup by file and line.
- Separate exact evidence from inferred relevance.
- No vague summaries when exact snippets fit budget.

### Phase 3: Rules, Skills, Workflows

Add registries:

- `RuleRegistry`
- `SkillRegistry`
- `WorkflowRegistry`

Keep their activation models separate.

### Phase 4: Agent Trace Store

Persist traces under `.charm/traces`.

Required behavior:

- Append-only JSONL or per-turn JSON.
- Link trace entries to session snapshots.
- Store command exit status and output hash.
- Store edit summaries and file paths.

### Phase 5: Delegation Controller

Upgrade the current broker into a robust child-session manager.

Required behavior:

- Child task spec.
- Assigned write scope.
- Status events.
- Result merge.
- Final verification gate.

### Phase 6: Session Insights

After completion, generate:

- Missed context.
- Repeated tool failures.
- Better rules/workflows.
- Suggested memory updates.
- Verification gaps.

Do not auto-commit durable memories without review.

## Design Rules for Charm

1. Keep model prompts short, but make context selection smart.
2. Treat context as evidence, not prose.
3. Separate rules, skills, workflows, memories, plans, and traces.
4. Make every tool call auditable.
5. Make every edit explainable by trace.
6. Prefer local worktree isolation before remote execution.
7. Make provider differences a render layer, not business logic.
8. Turn recurring user behavior into workflows, not bloated system prompts.

## Source Links

- Windsurf Cascade overview: https://docs.windsurf.com/windsurf/cascade
- Windsurf Memories: https://docs.windsurf.com/windsurf/cascade/memories
- Windsurf Rules: https://docs.windsurf.com/windsurf/cascade/rules
- Windsurf Skills: https://docs.windsurf.com/windsurf/cascade/skills
- Windsurf Workflows: https://docs.windsurf.com/windsurf/cascade/workflows
- Windsurf Fast Context: https://docs.windsurf.com/context-awareness/fast-context
- Devin in Windsurf: https://cognition.ai/blog/devin-in-windsurf
- How Cognition Uses Devin to Build Devin: https://cognition.ai/blog/how-cognition-uses-devin-to-build-devin
- Managed Devins: https://cognition.ai/blog/devin-can-now-manage-devins
- Scheduled Devins: https://cognition.ai/blog/devin-can-now-schedule-devins
- Agent Trace: https://cognition.ai/blog/agent-trace
- SWE-grep: https://cognition.ai/blog/swe-grep
- Devin API overview: https://docs.devin.ai/api-reference/overview
- Devin MCP: https://docs.devin.ai/work-with-devin/devin-mcp

