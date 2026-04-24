# 아키텍처 문서

## 개요

Charm은 모듈식 설계를 따르며, 각 컴포넌트는 명확한 책임을 가집니다.

장기적으로 Charm은 단순한 대화형 코딩 CLI가 아니라 evidence-first local agent harness를 지향합니다. 핵심 철학은 `docs/charm-strategy.md`에 정의되어 있으며, 아키텍처는 다음 원칙을 중심으로 진화합니다.

- 도구 호출은 기본값이며, 생략은 정책 gate가 판단합니다.
- 외부 API와 프레임워크 작업은 Context7 같은 docs MCP와 공식 레퍼런스를 우선합니다.
- 추상적인 요청은 실행 전 task contract로 구체화합니다.
- raw tool output은 trace에 보존하고, prompt에는 minified view만 전달합니다.
- 모든 편집은 evidence와 verification으로 추적 가능해야 합니다.

이 아키텍처의 목적은 모델이 강할 때만 잘 동작하는 에이전트가 아니라, 모델이 바뀌어도 일정 수준 이상의 개발 절차를 유지하는 하네스입니다. 따라서 모델 선택은 성능 최적화 계층이고, 정확성의 기본선은 `TaskConcretizer`, evidence 수집, reference gate, verification gate, trace store가 담당해야 합니다.

## 현재 구현 경계

현재 실행 가능한 기본 경로는 `SessionRuntime -> PromptAssembler -> ProviderClient -> ToolRegistry`입니다. 이 경로는 대화형 세션, 도구 호출, 승인 정책, TUI 렌더링을 제공하지만, 최종 목표인 gate-first 하네스를 완전히 강제하지는 않습니다.

일부 목표 컴포넌트는 이미 모듈로 존재하거나 스켈레톤이 있습니다.

- `TaskConcretizer`: task contract와 abstraction score의 시작점.
- `ReferenceBroker`: reference pack 구조와 provider fallback의 시작점.
- `TokenSaver`: raw output과 minified view 분리의 시작점.
- `PromptCompiler`: typed prompt section과 budget/provenance 모델의 시작점.

다음 아키텍처 작업의 핵심은 새 모듈을 늘리는 것이 아니라, 이 컴포넌트들을 세션의 필수 경로에 연결하는 것입니다.

## 모듈 구조

### `cli` (`src/cli.rs`)
- **책임**: 명령행 인자 파싱
- **기술**: `clap` derive macro
- **주요 기능**:
  - 상위 명령 (init, index, tools, exec, delegate)
  - 대화형 모드 (prompt 인자 또는 인자 없음)
  - 세션 관리 (--new, --continue, --session)
  - 모델/Provider 선택 (--model, --provider)

### `runtime` (`src/runtime/`)
- **`session_runtime.rs`**: 세션 라이프사이클 관리, TUI-에이전트 브리지
- **`router.rs`**: 메시지 라우팅 로직
- **`mcp.rs`**: MCP (Model Context Protocol) 구현
- **`workspace.rs`**: 워크스페이스 상태 관리

### `providers` (`src/providers/`)
- **`factory.rs`**: Provider 생성, 인증 해결, 모델 문자열 파싱
- **`client.rs`**: 공통 Provider 클라이언트 트레이트
- **구현체**:
  - `openai.rs`: OpenAI API
  - `anthropic.rs`: Anthropic Claude API
  - `google.rs`: Google Gemini API
  - `openrouter.rs`: OpenRouter API
  - `ollama.rs`: Ollama 로컬 API
  - `openai_codex.rs`: OpenAI Codex

### `tools` (`src/tools/`)
- **`mod.rs`**: 도구 레지스트리, 스키마 관리
- **도구 목록**:
  - `command.rs`: 셸 명령 실행
  - `file.rs`: 파일 읽기/쓰기/수정
  - `search.rs`: 코드베이스 검색 (rg 기반)
  - `browser.rs`: 웹 브라우징 (Playwright)
  - `web_search.rs`: 웹 검색
  - `github_cli.rs`: GitHub CLI 통합
  - `retrieval.rs`: 컨텍스트 검색
  - `semantic.rs`: 시맨틱 검색
  - `todo.rs`: TODO 관리
  - `notebook.rs`: Jupyter 노트북 지원
  - `test_runner.rs`: 테스트 실행
  - `rtk_filter.rs`: 코드 필터링

### `agent` (`src/agent/`)
- **`runner.rs`**: 에이전트 실행 오케스트레이션
- **`loop_agent.rs`**: 메인 에이전트 루프 (LLM ↔ 도구 호출)
- **`prompt.rs`**: 프롬프트 템플릿, 모드 관리 (Plan/Act/Build)
- **`task_concretizer.rs`**: 사용자 요청을 task contract와 abstraction score로 변환
- **`reference_broker.rs`**: 외부 문서, 패키지 소스, issue 검색 결과를 reference pack으로 정리
- **`token_saver.rs`**: command/test/search/docs output을 모델용 minified view로 축약
- **`prompt_compiler.rs`**: rules, memories, evidence, references, plan을 typed prompt section으로 컴파일
- **`context_compressor.rs`**: 긴 컨텍스트 압축

### `indexer` (`src/indexer/`)
- **`parser.rs`**: tree-sitter 기반 코드 파싱
- **`store.rs`**: 인덱스 저장/로드
- **`types.rs`**: 인덱스 데이터 구조
- **지원 언어**: Python, JavaScript, Rust (기본 지원)

### `harness` (`src/harness/`)
- **`checkpoint.rs`**: 실행 체크포인트 저장/복원
- **`memory.rs`**: 장기 메모리 관리

### `tui` (`src/tui/`)
- **`app.rs`**: Ratatui 기반 인터랙티브 UI
- **기능**: 스크롤링, 입력, 이벤트 핸들링

### `orchestrator` (`src/orchestrator/`)
- **`broker.rs`**: 멀티에이전트 작업 분해 및 실행
- **`roles.rs`**: Planner/Worker 역할 정의

## 데이터 흐름

```
[CLI] → [Provider Factory] → [Session Runtime] → [TUI]
                                           ↓
                                     [Agent Loop]
                                           ↓
                                    [Tools Registry]
                                           ↓
                                     [File/Command/Browser/...]
```

1. 사용자 입력 → CLI 파싱
2. Provider 해결 (모델 + 인증)
3. Session Runtime 부트 (TUI + Agent 초기화)
4. Agent Loop: LLM 호출 → 도구 선택 → 실행 → 결과 → LLM
5. TUI에 결과 표시

이 경로에서 모델은 아직 많은 판단을 직접 수행합니다. 목표 하네스는 모델이 판단하기 전에 런타임이 먼저 필요한 evidence, reference, verification 조건을 계산하도록 바꿉니다.

## 목표 하네스 흐름

현재 구현은 위 기본 흐름을 따르지만, 최종형은 다음 하네스 레이어를 목표로 합니다.

```
[CLI/TUI]
   ↓
[SessionRuntime]
   ↓
[IntentRouter]
   ↓
[TaskConcretizer]
   ↓
[ReferenceGate + ReferenceBroker]
   ↓
[FastContextWorker]
   ↓
[TokenSaver]
   ↓
[PromptCompiler]
   ↓
[ModelClient ↔ ToolRuntime]
   ↓
[VerificationGate]
   ↓
[AgentTraceStore]
   ↓
[SessionInsights]
```

### 핵심 목표 컴포넌트

- **TaskConcretizer**: 사용자 요청을 objective, scope, acceptance, verification, side effects가 있는 task contract로 변환합니다.
- **ReferenceBroker**: Context7, local package source, official docs, GitHub issues, StackOverflow 등을 통해 reference pack을 만듭니다.
- **FastContextWorker**: repo 내부 증거를 file/line/symbol 단위로 수집합니다.
- **TokenSaver**: command output, test log, docs, search results, code snippets를 모델용 minified view로 변환합니다.
- **PromptCompiler**: rules, memories, references, evidence, plan, tool policy를 typed prompt section으로 컴파일합니다.
- **AgentTraceStore**: raw output, evidence, tool calls, edits, tests, references를 연결해 audit/replay를 가능하게 합니다.
- **SessionInsights**: 반복 실패, 놓친 context, 필요한 rule/workflow/memory 후보를 추출합니다.

### 세션 경로에 들어가야 할 gate

- **Skip-Tool Gate**: repo/current-state claim이 있으면 tool use를 기본값으로 둡니다.
- **Concretization Gate**: 요청이 모호하면 task contract를 만들고, 필요한 경우 질문 하나만 합니다.
- **Reference Gate**: 외부 API, dependency, toolchain, 반복 실패 디버깅에는 공식 docs/source/issue evidence를 요구합니다.
- **Verification Gate**: 완료 선언 전에 테스트, 빌드, 타입체크, 수동 검증 중 가능한 증거를 요구합니다.
- **Trace Gate**: raw output, minified view, tool call, edit, verification 사이 연결을 보존합니다.

## 주요 디자인 패턴

### Provider Pattern
- 공통 `ProviderClient` 트레이트
- Factory에서 인증 해결 후 적절한 클라이언트 생성

### Tool Registry Pattern
- 런타임 도구 등록 및 실행
- JSON 스키마 기반 LLM 도구 정의

### Session Management
- 세션 ID 기반 상태 지속성
- --continue로 이전 세션 복원 가능
