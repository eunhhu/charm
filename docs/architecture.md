# 아키텍처 문서

## 개요

Charm은 모듈식 설계를 따르며, 각 컴포넌트는 명확한 책임을 가집니다.

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
