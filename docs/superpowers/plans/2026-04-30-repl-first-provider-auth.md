# REPL-First Provider Auth Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Provider credentials can be entered, saved, and used from inside the Charm TUI.

**Architecture:** Add a focused provider auth store under `src/providers/auth_store.rs`, wire it into provider resolution, and add a TUI auth overlay that saves credentials before opening the model picker. Keep runtime provider switching unchanged except that it now reads `~/.charm/auth.json`.

**Tech Stack:** Rust, serde JSON, ratatui/crossterm TUI, existing provider factory.

---

### Task 1: Auth Store

**Files:**
- Create: `src/providers/auth_store.rs`
- Modify: `src/providers/mod.rs`
- Modify: `src/providers/factory.rs`

- [x] Add `CharmAuthFile` and `StoredProviderAuth` with load/save helpers.
- [x] Save provider tokens to `~/.charm/auth.json` and set `0600` on Unix.
- [x] Resolve provider auth from env, then Charm auth store, then legacy Codex auth.
- [x] Test save/load and provider resolution with `CHARM_HOME`.

### Task 2: TUI Auth Wizard

**Files:**
- Modify: `src/tui/app.rs`

- [x] Add `Overlay::ProviderAuth`.
- [x] Add masked provider auth input state.
- [x] Open wizard from disconnected provider/model selection and `/provider connect <id>`.
- [x] Save credential and move into provider-filtered model picker.
- [x] Test overlay input, masking state, save transition, and local slash interception.

### Task 3: First-Run Fallback

**Files:**
- Modify: `src/main.rs`
- Modify: `src/runtime/session_runtime.rs`

- [x] Add unavailable runtime model that returns a provider setup error for chat calls.
- [x] Start TUI even when initial provider auth is missing.
- [x] Test no-auth startup path by constructing the fallback model.

### Task 4: Verification

**Files:**
- Modify: docs already touched by feature.

- [x] Run `cargo fmt`.
- [x] Run `rtk git diff --check`.
- [x] Run `rtk cargo check --all-targets`.
- [x] Run `rtk cargo test --all-targets`.
- [x] Run `rtk cargo clippy --all-targets` (0 errors, existing warnings remain).
- [ ] Run `rtk cargo clippy --all-targets -- -D warnings` (blocked by existing repository-wide lint warnings).
- [x] Run `rtk cargo build --release`.
- [x] Start `target/release/charm new` and `target/release/charm model ollama/qwen3-coder:30b` in a PTY and verify `/connect openrouter`, auth wizard, masked secret entry, model picker transition, and `CHARM_HOME/auth.json` save.
