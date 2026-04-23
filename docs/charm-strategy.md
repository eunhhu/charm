# Charm Strategy, Goals, and Philosophy

Date: 2026-04-23

## Product Definition

Charm is an evidence-first local agent harness for serious codebase work.

It is not trying to be only a prettier terminal chat client. Charm's long-term shape is an agent operating system for code: it gathers evidence, concretizes vague work, applies verified references, executes through tools, verifies results, and leaves an auditable trace.

Short version:

> Charm does maximal work internally, then compiles the smallest truthful context for the model.

Korean version:

> Charm은 내부적으로 10만큼 사고하고, 모델에게는 진실을 잃지 않는 최소 컨텍스트만 전달한다.

## Core Differentiation

Most coding agents start from a small context and expand only when the model decides it needs more.

Charm starts from the opposite assumption:

```text
Common agents:
  Start at 3 -> ask model whether to expand toward 10

Charm:
  Start from the 10-level workflow -> subtract unnecessary work safely
```

This means Charm should assume that code work needs tools, references, grounding, side-effect analysis, and verification unless a gate proves they can be skipped.

The goal is not wasteful depth. The goal is complete internal diligence plus minimal external representation.

## Design Principles

1. Context is evidence, not decoration.
2. Prompt is a compiled artifact, not a handwritten blob.
3. Tool use is the default. Skipping tools requires justification.
4. External API and framework work must be reference-first.
5. Ambiguous tasks must be concretized before execution.
6. Token cost is controlled by minified views, not by shallow reasoning.
7. Raw data must be preserved in traces; prompts receive compact, truthful views.
8. Every edit should point to evidence and verification.
9. Local-first execution is the default; remote/cloud delegation is optional.
10. Weak models should become useful through harness discipline.

## Competitive Positioning

| Agent | Strength | Charm's Differentiation |
| --- | --- | --- |
| Claude Code | Proactive behavior, tool use, mature coding workflow | Make proactivity explicit through concretization gates, reference gates, and auditable traces |
| Windsurf Cascade | IDE-local context and fast edit loop | Stay editor-neutral while keeping local-first codebase awareness |
| Devin | Async cloud execution and long-running PR work | Start local and auditable; add remote child agents later |
| OpenCode | Strong terminal UX and tool surface | Add deeper context compiler, reference-first policy, and traceability |
| Cursor | IDE-native UX and rules | Make rules, skills, workflows, memories, references, and traces separate primitives |
| Codex | Cloud tasks and code review | Offer local control plus optional delegation rather than cloud-only flow |

Charm should win when correctness, traceability, and codebase grounding matter more than chat convenience.

## The Execution Philosophy

### Tool-First, Not Tool-Optional

Model-chosen tool use is not enough. Models often skip tools when they feel confident, even when the repo or dependency state changed.

Charm should invert the question:

```text
Default: this work needs tools.
Question: is it safe to skip tools?
```

Tool use can be skipped only when the task is conversational, no current-state claim is made, and existing evidence is fresh enough.

Mandatory tool gates:

- Code edit before read/search: not allowed.
- Completion claim before verification: not allowed.
- External API claim without docs/reference: not allowed.
- Debugging after repeated failure without external precedent search: not allowed.
- Destructive or external side-effect action without risk policy: not allowed.

### Reference-First

Charm should not ask the model to remember APIs. It should ask the world first, then make the model adapt verified references to the current codebase.

Priority order:

1. Current repository source.
2. Installed package source and type definitions.
3. Official docs through Context7 or equivalent docs MCP.
4. Official GitHub issues, discussions, changelogs, and migration guides.
5. StackOverflow and reputable community fixes.
6. Model pretraining as last-resort background knowledge.

Reference-first is mandatory for:

- New framework or SDK usage.
- Dependency upgrades and migrations.
- Build/toolchain errors.
- Package-specific error messages.
- Provider SDK behavior.
- Anything involving current API shape.

Debugging policy:

```text
If local debugging fails for 2 fix cycles:
  stop guessing
  extract the exact error signature
  search external references
  compare known fixes
  apply the smallest verified fix
```

Context7 is a strong fit for this layer because it provides current, version-specific documentation and code examples through MCP. It should be treated as a first-class reference provider, not an optional convenience.

### Concretization Before Execution

Weak agents fail when they keep abstract tasks abstract. Charm should turn every request into a task contract before execution.

Task contract:

```text
Objective: what must be accomplished
Scope: what can be touched
Repo anchors: relevant files, symbols, tests, commands
Acceptance: what success means
Verification: how to prove success
Side effects: impacted areas and risks
Assumptions: what Charm will assume automatically
Open questions: what must be asked before proceeding
Depth: shallow / normal / deep / exhaustive
```

Abstraction score:

- `0.00-0.35`: concrete enough. Proceed with tool-first evidence.
- `0.35-0.70`: partially concrete. In auto mode, assume conservatively and inspect deeper. Otherwise ask one high-leverage question.
- `0.70-1.00`: too abstract. Do not execute. Decompose or ask for scope.

Important rule:

```text
High abstraction should increase grounding depth, not model freedom.
```

### 10-to-3 Optimization

Charm's internal workflow should be complete:

```text
concretize
-> retrieve code evidence
-> retrieve external references when needed
-> scan side effects
-> compile prompt
-> execute tools
-> verify
-> trace
-> extract insight
```

But the model should not receive all raw data. Charm should maintain the full state internally and provide a compact view.

```text
raw tool output
-> raw trace store
-> normalize
-> minify
-> select evidence
-> compile prompt sections
-> call model
```

This allows 10-level diligence without 10-level token waste.

## Token Saver Layer

Token saving is not optional. The 10-to-3 philosophy depends on it.

Charm needs a Token Saver layer that sits between every data source and the model:

```text
ToolResult(raw)
-> PreStoreHook: store raw output and hash
-> MinifyHook: create model-facing view
-> EvidenceHook: extract file/line/error/test metadata
-> BudgetHook: enforce prompt section limits
-> PromptCompiler
```

Rules by data type:

- Code: preserve line numbers and exact spans. Do not replace code with vague summaries.
- Errors: keep first actionable error, spans, failing test names, panic messages, and causal chain.
- Search results: dedupe by path and line, rank by relevance, cluster same-file hits.
- Docs: keep exact API signatures, minimal official examples, caveats, and version notes.
- Conversation: keep decisions, assumptions, open questions, and commitments.
- Tool history: keep args, status, metadata, and raw output reference. Drop repeated noise.

Raw data belongs in trace storage. Prompt data should be the smallest truthful view.

## Reference Pack

External documentation and issue search results should be compiled into a Reference Pack before entering the prompt.

```text
ReferencePack:
  source_kind: official_docs | github_issue | stackoverflow | blog | package_source
  library: package or framework name
  version: resolved version when available
  query: lookup query
  relevant_rules: exact rules or API behavior
  minimal_examples: short verified snippets
  caveats: known pitfalls or version changes
  anti_patterns: deprecated or wrong approaches
  source_refs: URLs, doc IDs, issue links
  confidence: high | medium | low
```

Reference Pack rules:

- Official docs outrank community answers.
- Current installed version outranks generic latest docs.
- Examples are adapted to the repo only after source and version are known.
- If references conflict, Charm should surface the conflict instead of averaging them.

## Core Runtime Gates

### Skip-Tool Gate

Allows no-tool responses only when:

- Intent is chat/meta.
- No repo-specific current-state claim is made.
- No file/API/version/build/test claim is made.
- Existing evidence is fresh.
- No edit or verification is needed.

### Reference Gate

Requires docs or external precedent when:

- The task touches third-party APIs.
- The task depends on current version behavior.
- The error signature comes from a framework, SDK, compiler, or toolchain.
- Two local fix attempts failed.

### Concretization Gate

Requires a task contract before implementation.

If required fields are missing, Charm must either:

- inspect the repo to infer them,
- make explicit conservative assumptions,
- or ask exactly one high-leverage question.

### Verification Gate

Completion claims require verification evidence:

- Tests for behavior changes.
- Build/check for compile-sensitive changes.
- Lint/typecheck when available.
- Manual reproduction notes when no automated test exists.

## Target Architecture

```text
TUI / CLI
  -> SessionRuntime
    -> IntentRouter
    -> TaskConcretizer
    -> ReferenceGate
    -> FastContextWorker
    -> TokenSaver
    -> PromptCompiler
    -> ModelClient
    -> ToolRuntime
    -> VerificationGate
    -> AgentTraceStore
    -> SessionInsights
```

Key modules to grow:

- `TaskConcretizer`: turns prompts into task contracts.
- `ReferenceBroker`: resolves library/package references through Context7, local package source, web search, and issue search.
- `FastContextWorker`: finds repo evidence before the main model acts.
- `TokenSaver`: minifies every tool/log/context payload.
- `PromptCompiler`: builds typed prompt sections under budget.
- `AgentTraceStore`: preserves raw facts and links them to edits.
- `SessionInsights`: turns repeated findings into candidate rules, workflows, and memories.

## Strategic Goals

### Near Term

- Establish the product philosophy in docs and prompts.
- Make tool-first and reference-first policies explicit.
- Add typed prompt section design.
- Add task contract schema.
- Add token saver interfaces for command, grep, test, docs, and file reads.

### Mid Term

- Integrate Context7 or equivalent docs MCP as the first reference provider.
- Add Reference Pack compilation.
- Implement abstraction scoring and task concretization.
- Add side-effect scan before edits.
- Store raw tool outputs and minified prompt views separately.

### Long Term

- Build local child-agent delegation with worktree isolation.
- Add remote/cloud runners after local traceability is strong.
- Add repo wiki/codemap generation.
- Add replayable agent traces.
- Use session insights to propose new rules, workflows, and memories.

## Success Metrics

| Metric | Target |
| --- | --- |
| Tool skip correctness | No skipped tool when repo/current-state evidence is required |
| Reference coverage | Third-party API changes cite docs or source |
| Debugging recovery | External precedent search happens before repeated blind fixes |
| Prompt efficiency | Raw output stored, prompt view reduced without losing spans |
| Edit traceability | Every edit links to evidence and verification |
| Clarification quality | Ask fewer questions, but ask when wrong execution risk is high |
| Verification discipline | No completion claim without check/test evidence when available |

## Source References

- Context7 CLI docs: https://context7.com/docs/clients/cli
- Context7 MCP listing: https://mcp.directory/servers/context7
- Context7 platform docs: https://context7.com/upstash/context7
- Devin/Windsurf harness research: `docs/devin-windsurf-harness-research.md`

