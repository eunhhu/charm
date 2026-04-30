# Provider 설정 가이드

Charm은 여러 AI provider를 지원하며, 각각 다른 인증 방식을 사용합니다.

## 지원 Providers

### OpenAI
- **환경 변수**: `OPENAI_API_KEY`
- **Codex 인증 파일**: `~/.codex/auth.json`의 `OPENAI_API_KEY` 필드도 사용 가능

```bash
export OPENAI_API_KEY="sk-..."
charm --model openai/gpt-4.1
```

### OpenAI Codex
- **인증 파일**: `~/.codex/auth.json` (Codex CLI와 공유)
- **환경 변수**: `CODEX_HOME`으로 경로 지정 가능
- **연결 방법**: `codex login`

```json
{
  "tokens": {
    "access_token": "your-token",
    "account_id": "workspace-id"
  }
}
```

### Anthropic (Claude)
- **환경 변수**: `ANTHROPIC_API_KEY`

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
charm --model anthropic/claude-sonnet-4-5
```

### Google (Gemini)
- **환경 변수**: `GEMINI_API_KEY` 또는 `GOOGLE_API_KEY`

```bash
export GEMINI_API_KEY="..."
charm --model google/gemini-2.5-pro
```

### OpenRouter
- **환경 변수**: `OPENROUTER_API_KEY`
- **기본값**: 설정되지 않은 경우 기본값으로 사용됨

```bash
export OPENROUTER_API_KEY="sk-or-..."
charm --model openrouter/moonshotai/kimi-k2.6  # 기본 모델
```

### Ollama (로컬)
- **환경 변수**: `OLLAMA_API_KEY` (선택사항, 기본값 "ollama")
- **요구사항**: 로컬 Ollama 서버 실행 중

```bash
charm --model ollama/qwen3-coder:30b
```

## 모델 지정 규칙

### Provider 접두사 (명시적)
```
{provider}/{model}
```

예시:
- `anthropic/claude-sonnet-4-5`
- `openai/gpt-4.1`
- `google/gemini-2.5-pro`
- `ollama/codellama:13b`

### 자동 감지 (암시적)
모델 이름에서 provider 자동 감지:
- `claude-*` → Anthropic
- `gpt-*`, `o1*`, `o3*`, `o4*`, `codex-*` → OpenAI
- `gemini-*` → Google
- `*:*` → Ollama
- 기타 → OpenRouter

## Provider 우선순위

1. 명시적 `--provider` 플래그
2. 모델 이름의 provider 접두사 (`provider/model`)
3. 모델 이름 기반 자동 감지
4. 환경 기본값 (OpenRouter)

## TUI Provider 연결

- `Ctrl+Shift+P` 또는 `/provider`로 provider 연결 상태를 확인합니다.
- 연결된 provider를 선택하면 해당 provider로 필터링된 model switcher가 열립니다.
- 미연결 provider를 선택하거나 `/provider connect <provider>`를 실행하면 설정 안내 modal이 열립니다.
- 세션 중 `/model provider/model-id`를 실행하면 pinned model metadata뿐 아니라 실제 provider client도 새 provider로 재바인딩됩니다.
- 긴 slash 출력(`/help`, `/audit`, `/evidence`, `/mcp`, `/lsp`)은 transcript 대신 scrollable modal로 표시됩니다.

## 오류 해결

### "API key must be set"
해당 provider의 환경 변수가 설정되어 있는지 확인하세요.

### "OpenAI Codex auth not found"
`~/.codex/auth.json` 파일이 존재하고 유효한지 확인하세요.
