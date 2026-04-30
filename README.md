# Charm

> **Evidence-first local agent harness + OPENCODE-grade TUI = auditable Rust CLI coding agent**

Charm은 Windsurf Cascade급 agent harness와 OPENCODE급 TUI/도구를 결합한 Rust 기반 AI 코딩 에이전트 CLI입니다. 대규모 코드베이스 작업을 위한 대화형 에이전트로, 자동화된 인덱싱, 도구 호출, 세션 관리, 멀티모델 지원을 제공합니다.

Charm의 장기 목표는 단순한 터미널 챗봇이 아니라 **근거 추적 가능한 로컬 코딩 에이전트 OS**입니다. Charm은 모델에게 API와 코드베이스를 기억하라고 시키지 않고, 도구와 레퍼런스로 검증된 근거를 가져온 뒤 그 근거를 현재 코드베이스에 맞게 적용합니다.

Charm의 핵심 제품 가치는 모델을 더 똑똑하게 보이게 만드는 것이 아니라 **어떤 모델을 쓰더라도 작업 저점을 높이는 것**입니다. 모델의 기억과 자신감에 기대지 않고, 런타임이 먼저 작업을 구체화하고, 근거를 모으고, 검증 가능한 프롬프트를 컴파일합니다.

---

## 목표

- **Cascade의 강점**: 뛰어난 코드베이스 이해, 컨텍스트 관리, 툴 호출 워크플로우
- **OPENCODE의 강점**: 강력한 TUI, 풍부한 도구 세트, 유연한 확장성
- **Charm의 차별성**: evidence-first, tool-first, reference-first, trace-first 하네스
- **결합**: 로컬에서 빠르게 실행되지만 모든 판단과 변경을 설명할 수 있는 AI 코딩 에이전트
- **모델 독립성**: 강한 모델은 더 빠르게 일하고, 약한 모델도 같은 gate와 evidence spine을 따라 최소 품질을 유지합니다.

## 철학

대부분의 에이전트는 작은 컨텍스트에서 시작해 모델이 필요하다고 판단할 때만 도구와 검색을 확장합니다. Charm은 반대로 동작합니다.

```text
일반 에이전트:
  3에서 시작 -> 필요하면 10까지 확장

Charm:
  10 수준의 작업 절차를 기본 가정 -> 필요 없는 것을 안전하게 감산
```

이 철학은 토큰 낭비를 의미하지 않습니다. Charm은 내부적으로는 충분히 깊게 조사하고, 모델에게는 `TokenSaver`와 `PromptCompiler`를 거친 최소한의 진실한 컨텍스트만 전달하는 방향을 지향합니다.

핵심 원칙:

- **Tool-first**: 코드, 현재 상태, 빌드, 테스트, 파일 내용에 대한 판단은 도구 사용이 기본값입니다. 도구 생략은 모델의 감이 아니라 skip gate가 허용해야 합니다.
- **Reference-first**: 외부 API, 프레임워크, 라이브러리 작업은 Context7 같은 docs MCP, 설치된 패키지 소스, 공식 문서를 우선합니다.
- **Concretize-first**: 추상적인 요청은 바로 실행하지 않고 목표, 범위, 성공 조건, 검증 방법이 담긴 task contract로 낮춥니다.
- **Token-minified**: raw output은 trace에 보존하고, 모델에게는 line/span/error를 잃지 않는 최소 evidence view만 전달합니다.
- **Verification-first completion**: 완료 선언은 테스트, 빌드, 타입체크, 수동 재현 같은 검증 근거 뒤에만 가능합니다.
- **Trace-first**: 모든 도구 호출, 편집, 테스트, 레퍼런스는 나중에 설명 가능해야 합니다.

### 저점을 높이는 실행 전략

Charm은 모델을 실행 주체로만 두지 않고, 하네스를 실행 통제층으로 둡니다.

```text
User request
-> TaskContract
-> EvidencePack
-> ReferencePack
-> PromptSections
-> Model/tool loop
-> Verification
-> Trace
```

이 구조에서 모델은 "모든 것을 기억하는 두뇌"가 아니라, Charm이 수집한 근거와 계약을 바탕으로 판단하는 실행 엔진입니다. 따라서 모델이 바뀌어도 작업 품질을 좌우하는 핵심 절차는 유지됩니다.

자세한 전략과 철학은 [`docs/charm-strategy.md`](docs/charm-strategy.md)를 보세요.

---

## 설치 및 빌드

### 요구사항
- Rust 1.85+ (2024 edition)
- Git

### 빌드
```bash
# 클론
git clone <repository-url>
cd charm

# 빌드
rtk cargo build --release

# 또는 개발 빌드
rtk cargo build
```

### 테스트
```bash
# 전체 테스트
rtk cargo test

# 체크만 (빠른 검증)
rtk cargo check
```

---

## 실행 예시

### 대화형 세션 시작
```bash
# 기본 모델로 시작 (기본: moonshotai/kimi-k2.6)
charm

# 프롬프트 직접 전달
charm "이 프로젝트 설명해줘"

# 특정 모델 사용
charm --model claude-sonnet-4-5 "리팩토링해줘"
charm --model openai/gpt-4.1 "버그 수정해줘"

# 새 세션 시작
charm --new

# 마지막 세션 이어하기
charm --continue

# 특정 세션 ID로 재개
charm --session <session-id>
```

### 워크스페이스 초기화
```bash
charm init
# .charm/ 디렉토리 생성
```

### 코드 인덱싱
```bash
charm index
# 워크스페이스 파일 및 심볼 인덱싱
```

### 사용 가능한 도구 목록
```bash
charm tools
```

### 도구 직접 실행
```bash
charm exec <tool-name> --args '{"key": "value"}'
```

### 작업 위임 (Delegate)
```bash
# 플래너 모델이 작업을 분해하고 실행
charm delegate "리팩토링: 에러 처리 개선"

# 커스텀 플래너 모델 지정
charm delegate "API 추가" --planner-model claude-opus-4
```

---

## Provider 인증 설정

Charm은 다음 AI providers를 지원합니다. 환경 변수로 인증을 설정하세요:

| Provider | 환경 변수 | 비고 |
|----------|-----------|------|
| **OpenAI** | `OPENAI_API_KEY` | OpenAI API 키 |
| **OpenAI Codex** | `CODEX_HOME` 또는 `~/.codex/auth.json` | Codex CLI 인증 파일 재사용 |
| **Anthropic** | `ANTHROPIC_API_KEY` | Claude API 키 |
| **Google (Gemini)** | `GEMINI_API_KEY` 또는 `GOOGLE_API_KEY` | Google AI API 키 |
| **OpenRouter** | `OPENROUTER_API_KEY` | 기본값 (moonshotai/kimi-k2.6) |
| **Ollama** | `OLLAMA_API_KEY` (선택) | 로컬 모델, 기본값 "ollama" |

### 인증 예시
```bash
# OpenAI
export OPENAI_API_KEY="sk-..."

# Anthropic
export ANTHROPIC_API_KEY="sk-ant-..."

# Google/Gemini
export GEMINI_API_KEY="..."
# 또는
export GOOGLE_API_KEY="..."

# OpenRouter (기본값)
export OPENROUTER_API_KEY="sk-or-..."

# Ollama (로컬)
export OLLAMA_API_KEY="ollama"  # 선택사항
```

### 모델 지정 방식
```bash
# provider/model 형식
charm --model anthropic/claude-sonnet-4-5
charm --model openai/gpt-4.1
charm --model google/gemini-2.5-pro
charm --model ollama/qwen3-coder:30b

# 자동 provider 감지
charm --model claude-...        # anthropic
charm --model gpt-...           # openai
charm --model gemini-...        # google
charm --model <name>:<tag>      # ollama
```

---

## 아키텍처 개요

```
charm/
├── cli.rs              # CLI 인자 파싱 (clap)
├── main.rs             # 진입점, 명령 라우팅
├── runtime/
│   ├── session_runtime.rs   # 세션 관리, TUI 통합
│   ├── router.rs            # 메시지 라우팅
│   ├── mcp.rs               # MCP 프로토콜
│   └── workspace.rs         # 워크스페이스 상태
├── providers/
│   ├── factory.rs           # Provider 팩토리, 인증 해결
│   ├── client.rs            # 공통 클라이언트 트레이트
│   ├── openai.rs            # OpenAI 구현
│   ├── anthropic.rs         # Anthropic 구현
│   ├── google.rs            # Google/Gemini 구현
│   ├── openrouter.rs        # OpenRouter 구현
│   ├── ollama.rs            # Ollama 로컬 모델
│   └── openai_codex.rs      # OpenAI Codex
├── tools/
│   ├── mod.rs               # 도구 레지스트리
│   ├── command.rs           # 셸 명령 실행
│   ├── file.rs              # 파일 조작
│   ├── search.rs            # 코드 검색
│   ├── browser.rs           # 웹 브라우징
│   ├── web_search.rs        # 웹 검색
│   ├── github_cli.rs        # GitHub CLI 통합
│   ├── retrieval.rs         # 컨텍스트 검색
│   └── ...
├── agent/
│   ├── runner.rs            # 에이전트 실행기
│   ├── loop_agent.rs        # 메인 루프
│   ├── prompt.rs            # 프롬프트 엔지니어링
│   ├── task_concretizer.rs  # 요청을 task contract로 구체화
│   ├── reference_broker.rs  # 외부/패키지 레퍼런스 pack 준비
│   ├── token_saver.rs       # raw output -> minified evidence view
│   ├── prompt_compiler.rs   # typed prompt section 컴파일
│   └── context_compressor.rs # 컨텍스트 압축
├── indexer/
│   ├── parser.rs            # 코드 파싱 (tree-sitter)
│   ├── store.rs             # 인덱스 저장
│   └── types.rs             # 인덱스 타입
├── harness/
│   ├── checkpoint.rs        # 실행 체크포인트
│   └── memory.rs            # 메모리 관리
├── tui/
│   └── app.rs               # Ratatui 기반 TUI
└── orchestrator/
    └── broker.rs            # 멀티에이전트 오케스트레이션
```

### 핵심 흐름

1. **CLI 파싱** (`cli.rs`): 명령행 인자 해석
2. **Provider 해결** (`providers/factory.rs`): 모델 문자열 → Provider + 인증
3. **세션 부트** (`runtime/session_runtime.rs`): TUI + 에이전트 초기화
4. **에이전트 루프** (`agent/`): LLM 호출 → 도구 실행 → 응답
5. **TUI 렌더링** (`tui/`): Ratatui로 인터랙티브 인터페이스

이 기본 흐름은 현재 실행 가능한 제품 뼈대입니다. 아래 최종형 하네스 흐름은 이 뼈대 위에 강제 gate와 evidence compiler를 끼워 넣는 방향입니다.

### 최종형 하네스 흐름

```text
User task
-> TaskConcretizer
-> ReferenceGate
-> FastContextWorker
-> TokenSaver
-> PromptCompiler
-> Model/tool loop
-> VerificationGate
-> AgentTraceStore
-> SessionInsights
```

이 흐름은 아직 전부 구현된 최종 상태가 아니라 Charm이 향해야 할 제품 설계 방향입니다.

현재 일부 컴포넌트는 스켈레톤 또는 부분 구현 상태입니다. 중요한 다음 단계는 새 개념을 더 추가하는 것이 아니라, 이미 있는 `TaskConcretizer`, `ReferenceBroker`, `TokenSaver`, `PromptCompiler`를 세션 런타임의 필수 경로로 연결하는 것입니다.

---

## 현재 상태

### 최근 구현된 핵심 경로

- **Task contract 기본 경로**: 세션 입력이 `TaskConcretizer`를 거쳐 목표, 수용 조건, 검증 조건을 가진 contract로 낮아집니다.
- **Reference-first 경로**: `ReferenceBroker`가 Context7, local package source, registry, web fallback 순서로 `ReferencePack`을 준비합니다. outbound HTTP는 URL guard와 DNS pinning을 통과해야 합니다.
- **Prompt compiler 경로**: `PromptAssembler`가 task contract, reference pack, verification gate, autonomy policy를 typed prompt section으로 렌더링합니다.
- **Trace/minification 경로**: command full output은 `.charm/logs/commands/<command_id>.log`에 저장하고 `log_ref`/`output_hash`/byte count를 tool metadata와 trace에 남깁니다. 모델-facing view는 `TokenSaver`/RTK filter로 줄이되 full log reference는 유지합니다.
- **Runtime gates**: 파일 편집 전 repo evidence gate, completion claim 전 verification gate, 위험도 기반 approval gate가 세션 런타임에서 동작합니다.
- **Command lifecycle**: non-blocking command는 `poll_command`로 조회하고 `cancel_command`로 취소할 수 있습니다.
- **Read path cache**: `ToolRegistry`의 `read_range`는 `.charm/cache/file-cache.json` backed `FileCache`를 사용해 세션 간 반복 파일 읽기 비용을 줄입니다.
- **Evidence browser**: `/evidence`, `/evidence repo`, `/evidence refs`가 저장된 세션 evidence/reference pack을 `.charm/sessions/<id>/`에서 읽어 요약합니다.
- **Sub-agent export**: `/agent export <id>`가 sub-agent worktree status/diff/file snapshot을 `.charm/exports/` markdown artifact로 저장합니다.
- **Sub-agent PR draft**: `/agent pr <id>`가 remote 인증 없이 검토 가능한 PR title/body/test plan markdown을 생성합니다.

### 빌드 상태 확인
```bash
# 컴파일 체크
rtk cargo check

# 테스트 실행
rtk cargo test
```

### 알려진 제한사항

- **Rust 심볼 인덱싱**: 함수, 구조체, enum, impl 메서드 지원 (매크로, const/static, 고급 trait/generic 처리는 제한적)
- **컴파일 워닝**: 다수의 unused variable/function 경고 존재
- **Fast Executor**: `AgentLoop`와 TUI `SessionRuntime`의 독립 read-only batch 경로에 연결됨. TUI는 mixed tool-call 응답에서도 consecutive read/search chunk를 병렬 실행하고, start/finish event ordering, trace, provider `tool_call_id` alignment를 보존하며 mutating/approval-gated tools는 순차 실행합니다. 실패/정책차단/실행중 mutation 뒤의 추가 mutation은 `mutating_scheduler` synthetic result로 차단해 partial write 확산을 막습니다.
- **캐시 시스템**: `ToolCache`는 검색류 결과, `FileCache`는 persistent `read_range` 파일 읽기에 적용됩니다.
- **Delegate 모드**: 실험적 기능

### 개발 중인 기능
- 컨텍스트 압축 최적화
- 멀티모달 입력 지원
- MCP 서버 통합 확장

---

## 라이선스

[라이선스 정보 추가 예정]

---

## 기여

이 프로젝트는 Windsurf Cascade의 워크플로우에서 영감을 받아 개발되었습니다.
