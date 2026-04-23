# Charm

> **Cascade-grade agent harness + OPENCODE-grade TUI = 최강 Rust CLI coding agent**

Charm은 Windsurf Cascade급 agent harness와 OPENCODE급 TUI/도구를 결합한 Rust 기반 AI 코딩 에이전트 CLI입니다. 대규모 코드베이스 작업을 위한 대화형 에이전트로, 자동화된 인덱싱, 도구 호출, 세션 관리, 멀티모델 지원을 제공합니다.

---

## 목표

- **Cascade의 강점**: 뛰어난 코드베이스 이해, 컨텍스트 관리, 툴 호출 워크플로우
- **OPENCODE의 강점**: 강력한 TUI, 풍부한 도구 세트, 유연한 확장성
- **결합**: 최고의 AI 코딩 에이전트 하네스

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

---

## 현재 상태

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
- **Fast Executor**: 일부 코드 미연결 가능성
- **캐시 시스템**: 부분적 구현 상태
- **Delegate 모드**: 실험적 기능

### 개발 중인 기능
- 향상된 Rust 지원 (tree-sitter-rust 기반)
- 컨텍스트 압축 최적화
- 멀티모달 입력 지원
- MCP 서버 통합 확장

---

## 라이선스

[라이선스 정보 추가 예정]

---

## 기여

이 프로젝트는 Windsurf Cascade의 워크플로우에서 영감을 받아 개발되었습니다.