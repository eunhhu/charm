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
- `docs/devin-windsurf-harness-research.md`: Devin/Windsurf 하네스 분석과 Charm 이식 방향
- `OPTIMIZATION_ROADMAP.md`: 구현 우선순위와 성능 목표

코드 변경이 다음 영역에 닿으면 관련 문서도 같이 업데이트하세요.

- `PromptAssembler`, `SessionRuntime`, `ToolRegistry`: prompt/compiler/tool policy 문서 확인
- `indexer`, `retrieval`: evidence/context 전략 확인
- `providers`, `runtime/mcp`: reference-first와 MCP 전략 확인
- `tools/command`, `tools/search`, `tools/test_runner`: TokenSaver/minification 전략 확인
- `orchestrator`: delegation 전략 확인

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
`tools/fast_executor.rs`의 일부 기능이 미연결 상태일 수 있습니다.

## 기여 가이드라인

1. 새로운 provider 추가 시 `providers/factory.rs`에 등록
2. 새로운 도구 추가 시 `tools/mod.rs`에 등록
3. CLI 변경 시 `README.md`와 `docs/` 업데이트
4. 외부 API/SDK 동작을 구현할 때는 공식 문서나 Context7 같은 reference source를 확인하고 source URL을 PR/문서/trace에 남기기
5. 코딩 작업 자동화 기능은 raw output 보존과 model-facing minified view 분리를 고려하기
