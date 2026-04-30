# REPL-First Provider Auth Design

## Goal

Charm provider setup must complete inside the TUI/REPL, without telling users to leave and export shell variables.

## Storage

Provider credentials are stored under `~/.charm/auth.json`. Tests may override the home root with `CHARM_HOME`, but product behavior uses the user's `~/.charm` directory. The auth file is created with owner-only permissions on Unix.

## UX

Provider connection is an action, not documentation:

- `/provider` opens provider status.
- Selecting a disconnected provider opens an in-place auth wizard.
- `/provider connect <provider>` opens the same wizard directly.
- The wizard masks input, saves the credential, and opens the provider-filtered model picker.
- Selecting a disconnected model opens the same wizard for that provider.
- `/model provider/model` uses credentials from `~/.charm/auth.json` immediately.

## Auth Resolution

Credential lookup order:

1. Environment variables.
2. `~/.charm/auth.json`.
3. Legacy `~/.codex/auth.json` fallback for Codex/OpenAI compatibility.
4. Provider-specific unauthenticated behavior, such as Ollama's local default token.

## First Run

If the initial provider cannot authenticate, Charm still starts the TUI with an unavailable placeholder model. The user can then open provider setup from inside the REPL.

## Scope

This phase implements provider auth. The same modal/wizard pattern should later be reused for MCP setup, settings, and other user-visible recovery flows.
