# Charm 전략 및 최적화 로드맵

## 목표

Charm의 목표는 Windsurf Cascade급 harness와 OPENCODE급 TUI를 넘어서, 근거 추적 가능한 로컬 코딩 에이전트 OS가 되는 것입니다.

핵심 철학:

```text
일반 에이전트: 3에서 시작 -> 필요하면 10까지 확장
Charm: 10 수준의 절차를 기본 가정 -> 필요 없는 것을 안전하게 감산
```

이를 위해 Charm은 다음 원칙을 따른다:

- **Tool-first**: 코딩 작업은 도구 사용이 기본값이며, 도구 생략은 gate가 증명해야 한다.
- **Reference-first**: 외부 API/프레임워크/라이브러리는 Context7 같은 docs MCP와 공식 레퍼런스를 우선한다.
- **Concretize-first**: 추상 요청은 task contract로 낮춘 뒤 실행한다.
- **Token-minified**: 내부적으로 깊게 조사하되, 모델에는 최소한의 진실한 context view만 전달한다.
- **Trace-first**: 모든 근거, 도구 호출, 편집, 검증 결과는 추적 가능해야 한다.

상세 철학은 `docs/charm-strategy.md`를 기준 문서로 삼는다.

## 현재 전략 판단

Charm의 가장 큰 레버는 새 모델을 더 많이 붙이는 것이 아니라, 모델이 바뀌어도 유지되는 실행 spine을 세션 런타임에 강제하는 것입니다.

```text
TaskContract
-> EvidencePack
-> ReferencePack
-> PromptSections
-> Tool loop
-> Verification
-> Trace
```

강한 모델은 이 spine 위에서 더 빠르고 넓게 추론합니다. 약한 모델은 같은 spine 덕분에 파일 읽기, reference 확인, 테스트 실행, 완료 근거 제시 같은 기본 절차를 놓치기 어렵습니다.

## 상태 표기

- **Designed**: 문서/타입 설계가 있음.
- **Scaffolded**: Rust 모듈 또는 구조체가 있으나 기본 세션 경로에 아직 강제되지 않음.
- **Wired**: 일반 사용자 세션에서 자동 적용됨.
- **Enforced**: 모델이 생략하려 해도 gate가 막음.

## 현재 진행 요약

- **완료/Wired**: TaskContract 생성, repo evidence 수집, ReferencePack 수집, PromptCompiler section rendering, TokenSaver minified trace, approval gate, repo evidence gate, verification gate, repeated-failure precedent gate, GitHub issue/discussion precedent provider, source-aware write/command-target scope guard, command cancel, persistent read-range FileCache, TokenSaver-backed `/compact`, slash audit/replay UI, TUI read-only parallel tool execution.
- **부분/Wired**: Context7/local package/registry/web reference provider, trace linkage, side-effect scope inference.
- **남음**: mutating tool scheduling 고도화, persistent evidence browser, sub-agent result PR/export workflow.

## 제품 레이어 로드맵

### Phase 0: 하네스 철학 정착

- [x] Evidence-first / Reference-first / Tool-first 원칙 문서화
- [x] Devin/Windsurf harness research 문서화
- [x] 현재 `PromptAssembler`와 runtime prompt에 철학 반영
  - Status: Wired. task contract, repo evidence, reference pack, verification gate가 session system prompt에 들어감.
- [x] tool skip 기준과 reference-required 기준을 정책으로 분리
  - Status: Enforced for repo evidence/edit, reference-required prompt packs, verification claim, approval risk. 더 정교한 skip classifier는 남음.

### Phase 1: Task Concretization Layer

목표: 추상 요청을 실행 가능한 `TaskContract`로 변환한다.

```rust
pub struct TaskContract {
    pub objective: String,
    pub scope: Vec<String>,
    pub repo_anchors: Vec<String>,
    pub acceptance: Vec<String>,
    pub verification: Vec<String>,
    pub side_effects: Vec<String>,
    pub assumptions: Vec<String>,
    pub open_questions: Vec<String>,
    pub depth: ExecutionDepth,
}
```

- [x] abstraction score 계산
  - Status: Wired. `TaskConcretizer::concretize_for_auto`가 세션 턴 준비 경로에서 호출됨.
- [x] missing contract fields 추출
  - Status: Wired. objective/scope/acceptance/verification/assumptions/open questions 기본 추출.
- [x] ask / inspect / auto-assume / execute 결정
  - Status: Partially Wired. 현재 자동 세션은 conservative assumption + evidence collection 중심. 사용자 질의 interrupt 정책은 추가 필요.
- [x] side-effect scan 추가
  - Status: Wired. 요청 surface에서 TUI/keybinding, session/runtime, auth/provider, schema, CLI, cache/index, dependency/API, destructive risk를 contract에 기록하고, concrete scope(`src/tui/**`, `src/runtime/**` 등)를 추론.
- [x] side-effect 기반 scope guard 1차
  - Status: Wired for writes and explicit stateful command targets. `EditPatch`/`WriteFile` target 또는 stateful/destructive/external `RunCommand`의 path-like target이 current `TaskContract.scope`, `repo_anchors`, `repo_evidence` 밖이면 approval queue/registry 실행 전 `scope_guard`로 차단하고 trace에 남김.

### Phase 2: Reference-First Layer

목표: 모델 pretraining이 아니라 최신 레퍼런스와 실제 예제를 우선한다.

- [x] Context7 MCP 연결을 first-class reference provider로 지원
  - Status: Wired. Context7 endpoint 사용 시 HTTPS + URL/DNS guard를 통과한 POST만 허용.
- [x] local package source/type definitions 조회
  - Status: Wired. vendored/local package source roots 우선 조회.
- [x] registry/web search fallback 정책
  - Status: Wired. npm/PyPI/crates.io + web search fallback 존재. GitHub issue/discussion 전용 precedent search도 repeated-failure gate에 연결됨.
- [x] `ReferencePack` 구조 도입
  - Status: Wired. `ReferenceBroker`가 provider 결과를 `ReferencePack`으로 컴파일하고 session prompt에 주입.
- [x] 두 번 이상 local debugging 실패 시 external precedent search 강제
  - Status: Wired. 연속 command failure 2회 이후 GitHub issue search와 package repo 기반 GitHub Discussions GraphQL 결과를 `GitHubIssues` precedent pack과 trace event로 주입.

```rust
pub struct ReferencePack {
    pub source_kind: ReferenceSourceKind,
    pub library: Option<String>,
    pub version: Option<String>,
    pub query: String,
    pub relevant_rules: Vec<String>,
    pub minimal_examples: Vec<String>,
    pub caveats: Vec<String>,
    pub anti_patterns: Vec<String>,
    pub source_refs: Vec<String>,
    pub confidence: ReferenceConfidence,
}
```

### Phase 3: Token Saver Layer

목표: 10 수준의 내부 조사와 3-6 수준의 prompt footprint를 동시에 달성한다.

- [x] raw output store와 minified prompt view 분리
  - Status: Wired. `SessionRuntime::record_tool_result`가 raw output과 minified output을 함께 trace.
- [x] command/test/rustc/cargo output minifier
  - Status: Wired for command/cargo/test source kinds. coverage 확대 필요.
- [x] grep/search result dedupe and ranking
  - Status: Partially Wired. repo evidence 수집에서 rank sort와 path/line dedupe 적용.
- [x] docs/reference snippet distiller
  - Status: Partially Wired. ReferencePack compile 단계에서 rules/examples/caveats/source refs 추출.
- [x] code span minifier with line number preservation
  - Status: Partially Wired. line/span 중심 minification 존재. provenance 정밀도 개선 필요.
- [x] prompt section budget enforcement
  - Status: Wired. `PromptCompiler`가 section budget과 전체 budget을 적용.

```rust
pub struct MinifyRequest {
    pub source_kind: SourceKind,
    pub raw_ref: RawRef,
    pub raw: String,
    pub intent: RouterIntent,
    pub budget: TokenBudget,
    pub preserve: PreservePolicy,
}

pub struct MinifiedView {
    pub text: String,
    pub evidence: Vec<EvidenceRef>,
    pub omissions: Vec<Omission>,
    pub raw_ref: RawRef,
}
```

### Phase 4: Prompt Compiler

목표: prompt를 문자열 조립이 아니라 typed artifact로 만든다.

- [x] `PromptSection` 도입
  - Status: Wired.
- [x] priority, activation, token budget, provenance 관리
  - Status: Wired in session system prompt.
- [x] provider-specific rendering
  - Status: Scaffolded. provider hint 기반 rendering 시작점 있음.
- [x] section snapshot tests
  - Status: Basic tests present. provider별 golden snapshot 확대 필요.

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

### Phase 5: Agent Trace and Session Insights

목표: 모든 판단과 편집을 설명 가능하게 만든다.

- [x] `.charm/traces` raw event store
- [x] evidence -> tool call -> edit -> verification linkage
  - Status: Partially Wired. turn_id 기반 trace와 verification update 존재. edit diff 단위 linkage 강화 필요.
- [ ] command output hash and full log ref
- [ ] repeated failure / missed context / missing reference 분석
- [ ] candidate rules, workflows, memories 제안

## 성능 최적화 로드맵

### Phase A: 토큰 관리 고도화 (레이턴시 30% 감소 예상)

#### 1.1 정확한 토큰 카운팅
- [ ] `tiktoken-rs` 또는 `tokenizers` 통합
- [ ] 모델별 토크나이저 선택 (cl100k_base, o200k_base)
- [ ] 실시간 컨텍스트 윈도우 모니터링

```rust
// 개선 방향
pub struct TokenCounter {
    tokenizer: Tokenizer,  // tiktoken
}

impl TokenCounter {
    pub fn count(&self, text: &str) -> usize {
        self.tokenizer.encode(text, true).len()
    }
    
    pub fn count_messages(&self, msgs: &[Message]) -> usize {
        // OpenAI 형식의 정확한 토큰 계산
    }
}
```

#### 1.2 스마트 컨텍스트 압축
- [ ] 중요도 기반 메시지 순위화
- [ ] 요약이 아닌 "키 포인트 추출"
- [ ] 코드 스니펫은 보존, 중간 대화는 압축

```rust
pub enum CompressionStrategy {
    PreserveCode,      // 코드 블록은 항상 유지
    SummarizeText,     // 일반 텍스트는 요약
    DropOldToolOutput, // 오래된 tool 결과 제거
}
```

### Phase B: 캐싱 및 인덱싱 (레이턴시 40% 감소 예상)

#### 2.1 도구 스키마 캐싱
```rust
pub struct CachedToolSchemas {
    schemas: OnceCell<Vec<ToolSchema>>,
    json_strings: OnceCell<Vec<String>>,  // 미리 직렬화
}
```

#### 2.1b 파일 읽기 캐싱
- [x] registry-local `FileCache`를 `read_range` 경로에 연결
- [x] 파일 수정 시간 기반 stale 검사
- [x] `write_file` 성공 시 해당 path cache invalidate
- [x] session 간 persistent file cache

#### 2.2 임베딩 기반 시맨틱 검색
- [ ] `fastembed-rs` 통합 (로컬 임베딩)
- [ ] FAISS 인덱스 빌드
- [ ] 증분 인덱싱 (변경된 파일만 재처리)

```rust
pub struct SemanticIndex {
    model: FastEmbedModel,
    faiss_index: IndexFlatIP,  // 내적 기반 검색
    file_hashes: HashMap<PathBuf, u64>,  // 변경 감지
}
```

#### 2.3 코드 인덱스 메모리 매핑
- [ ] `memmap2` 활용한 대형 인덱스 로딩
- [ ] 백그라운드 인덱싱 워커

### Phase C: 스트리밍 및 동시성

#### 3.1 스트리밍 지원
```rust
pub enum ChatResponse {
    Streaming(Stream<Item=TokenChunk>),
    Blocking(Message),
}
```

#### 3.2 병렬 도구 실행
```rust
// 독립적인 도구 호출은 동시 실행
let futures = tool_calls.iter().map(|call| {
    tokio::spawn(execute_tool(call))
});
let results = join_all(futures).await;
```

- [x] `FastExecutor`를 `AgentLoop` read-only batch 경로에 연결
- [x] mutating/shell tools는 ordered tail로 분리
- [x] TUI `SessionRuntime` 병렬 실행 연결
  - Status: Wired for safe read/search batches. `SessionRuntime` now emits all starts before batch execution, records ordered tool results with original `tool_call_id`s, traces `parallel_tool_batch`, and keeps mutating/approval-gated tools ordered.

### Phase D: RTK (Retrieval Toolkit) 고도화

현재 `rtk_filter.rs`는 있지만 미사용 중. OPENCODE급 기능 추가:

```rust
pub struct RTKQuery {
    pub intent: QueryIntent,  // explore, plan, implement, verify
    pub depth: SearchDepth,   // quick, deep, exhaustive
    pub context_budget: usize, // 토큰 예산
}

pub async fn intelligent_retrieval(
    query: &RTKQuery,
    codebase: &CodebaseIndex,
) -> RetrievalResult {
    // 질의 의도에 따른 검색 전략 선택
    match query.intent {
        Explore => parallel_search_with_ranking(query),
        Plan => dependency_graph_traversal(query),
        Implement => precise_symbol_lookup(query),
        Verify => test_and_example_search(query),
    }
}
```

### Phase E: 메모리 시스템 강화

#### 5.1 계층적 메모리
```rust
pub struct HierarchicalMemory {
    working_memory: Vec<Turn>,      // 현재 세션
    short_term: Vec<SessionSummary>, // 최근 세션
    long_term: EmbeddedMemories,    // 벡터 DB 저장
}
```

#### 5.2 자동 메모리 커밋
- [ ] 중요한 도구 실행 결과 자동 기록
- [ ] 세션 종료 시 요약 자동 생성

## 구현 우선순위

### 즉시 구현 (High Impact, Low Effort)
1. ~~`PromptAssembler`의 tool policy를 strategy와 맞춤~~
2. ~~`TaskConcretizer`를 세션 입력 경로에 연결~~
3. ~~`TokenSaver`를 command/search/test tool result 경로에 연결~~
4. ~~`PromptCompiler`를 session system prompt 생성 경로에 연결~~
5. ~~tool skip/reference/verification gate를 runtime policy로 분리~~

### 단기 구현 (1-2주)
6. ~~ReferencePack provider fetch 구현(Context7/local package source 우선)~~
7. FastContextWorker evidence format
8. ~~AgentTraceStore raw/minified 분리~~
9. ~~VerificationGate와 완료 선언 정책~~
10. ~~컨텍스트 압축 개선(TokenSaver-backed `/compact`)~~

### 중기 구현 (2-4주)
11. 도구 스키마 캐싱과 병렬 tool execution 안정화
12. 임베딩 기반 시맨틱 검색
13. RTK 고도화
14. 계층적 메모리 시스템
15. worktree-isolated child-agent delegation 강화

## 성능 목표

| 지표 | 현재 | 목표 | 개선률 |
|------|------|------|--------|
| 첫 응답 시간 | 2-3s | <1s | 60%↓ |
| 컨텍스트 압축 정확도 | ~70% | >95% | 35%↑ |
| 시맨틱 검색 정확도 | ~50% | >85% | 70%↑ |
| 대형 코드베이스 인덱싱 | 30s+ | <5s | 80%↓ |
| 메모리 사용량 | 높음 | 최적화 | 40%↓ |
| 외부 API 작업 레퍼런스 커버리지 | 낮음 | >90% | 신규 |
| completion claim 검증률 | 불명확 | >95% | 신규 |
| 편집 trace 가능성 | 낮음 | >95% | 신규 |
