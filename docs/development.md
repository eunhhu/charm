# 개발 가이드

## 빌드 및 테스트

### 빠른 체크
```bash
rtk cargo check
```

### 전체 빌드
```bash
rtk cargo build
rtk cargo build --release
```

### 테스트
```bash
rtk cargo test
```

### 특정 테스트
```bash
rtk cargo test cli::tests
rtk cargo test providers::
```

## 프로젝트 구조

```
src/
├── cli.rs              # CLI 인자 정의 (수정 시 README 동기화 필요)
├── main.rs             # 진입점
├── lib.rs              # 라이브러리 진입점
├── agent/              # 에이전트 로직
├── context/            # 컨텍스트 관리
├── core/               # 핵심 타입
├── harness/            # 실행 하네스
├── indexer/            # 코드 인덱싱
├── orchestrator/       # 멀티에이전트 오케스트레이션
├── prism/              # 프리즘 모듈
├── providers/          # LLM providers
├── retrieval/          # 검색/리트리버
├── runtime/            # 런타임/세션 관리
├── tools/              # 도구 구현
└── tui/                # 터미널 UI
```

## 설계 기준 문서

제품 철학과 장기 설계 방향은 다음 문서를 기준으로 합니다.

- `docs/charm-strategy.md`: Charm의 목표, 철학, 차별성, tool-first/reference-first/token-minified 전략
- `docs/architecture.md`: 현재 실행 경로와 목표 하네스 경로의 경계
- `docs/devin-windsurf-harness-research.md`: Devin/Windsurf 하네스 분석과 Charm 이식 방향
- `OPTIMIZATION_ROADMAP.md`: 구현 우선순위와 성능 목표

코드 변경이 다음 영역에 닿으면 관련 문서도 같이 업데이트하세요.

- `PromptAssembler`, `PromptCompiler`, `SessionRuntime`, `ToolRegistry`: prompt/compiler/tool policy 문서 확인
- `TaskConcretizer`, `runtime/router`: task contract, abstraction score, autonomy/gate 정책 확인
- `indexer`, `retrieval`: evidence/context 전략 확인
- `ReferenceBroker`, `providers`, `runtime/mcp`: reference-first와 MCP 전략 확인
- `TokenSaver`, `tools/command`, `tools/search`, `tools/test_runner`: raw output 보존과 minified view 전략 확인
- `orchestrator`: delegation 전략 확인

## 하네스 개발 우선순위

새 기능을 추가하기 전에 다음 질문을 먼저 확인하세요.

1. 이 변경이 모델의 기억에 의존하는가, 아니면 evidence를 수집하는가?
2. tool/reference/verification을 생략해도 안전한 조건이 코드로 표현되어 있는가?
3. raw output과 model-facing summary가 분리되어 있는가?
4. 완료 선언을 뒷받침할 검증 경로가 있는가?

현재 가장 중요한 방향은 이미 세션 런타임에 연결된 `TaskConcretizer`, `ReferenceBroker`, `TokenSaver`, `PromptCompiler`, read-only parallel execution을 더 엄격한 gate와 더 좋은 UX로 다듬는 것입니다. 새 추상화를 늘리기보다 다음을 우선합니다.

- mutating tool scheduling은 ordered execution을 유지하면서 더 정교하게 다듬기

## 의존성

### 핵심
- `tokio`: 비동기 런타임
- `clap`: CLI 파싱
- `anyhow`/`thiserror`: 에러 처리

### LLM/Networking
- `reqwest`: HTTP 클라이언트
- `schemars`: JSON 스키마

### 파싱
- `tree-sitter-*`: 코드 파싱

### TUI
- `ratatui`: 터미널 UI
- `crossterm`: 터미널 제어

### 기타
- `serde`/`serde_json`: 직렬화
- `git2`: Git 통합
- `walkdir`/`ignore`: 파일 순회
- `fantoccini`: 브라우저 자동화

## 디버깅

### 로그 활성화
```bash
RUST_LOG=debug charm ...
```

### 세션 디버깅
세션 파일은 `.charm/sessions/`에 저장됩니다.

## 알려진 이슈

### 컴파일 워닝
현재 다수의 unused 코드 워닝이 존재합니다. 작업 중인 모듈은 정상입니다.

### Rust 인덱싱
`indexer/parser.rs`의 Rust 심볼 인덱싱이 구현되었습니다:
- 지원: 함수, 구조체, enum, impl 메서드
- 제한사항: 매크로, const/static, 고급 trait/generic 처리

### Fast Executor
`tools/fast_executor.rs`는 `AgentLoop`와 TUI `SessionRuntime`의 read-only batch 경로에 연결되어 있습니다. TUI runtime은 safe read/search batch만 병렬 실행하고, start/finish event order, trace event, provider `tool_call_id` alignment를 보존합니다. Mutating, shell, approval-gated calls는 순차 실행을 유지합니다.

## 기여 가이드라인

1. 새로운 provider 추가 시 `providers/factory.rs`에 등록
2. 새로운 도구 추가 시 `tools/mod.rs`에 등록
3. CLI 변경 시 `README.md`와 `docs/` 업데이트
4. 외부 API/SDK 동작을 구현할 때는 공식 문서나 Context7 같은 reference source를 확인하고 source URL을 PR/문서/trace에 남기기
5. 코딩 작업 자동화 기능은 raw output 보존과 model-facing minified view 분리를 고려하기
