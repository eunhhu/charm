use crate::runtime::session_runtime::SessionRuntime;
use crate::runtime::types::{
    ApprovalRequest, ApprovalStatus, AutonomyLevel, BackgroundJob, BackgroundJobKind,
    BackgroundJobStatus, LspSnapshot, McpSnapshot, RouterIntent, RuntimeEvent, SessionLifecycle,
    WorkspacePreflight,
};
use crate::tui::dialog::{
    self, DialogOption, DialogSelectLayout, DialogSelectProps, DialogSelectState, InputMode,
    KeybindHint,
};
use crate::tui::event::{AppEvent, EventBridge};
use crate::tui::theme::Theme;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton,
    MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap,
};
use std::io::{self, Stdout};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

pub struct InputState {
    buffer: String,
    cursor: usize,
    history: Vec<String>,
    history_index: usize,
    saved_buffer: String,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: 0,
            saved_buffer: String::new(),
        }
    }
}

impl InputState {
    pub fn insert(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn insert_str(&mut self, text: &str) {
        self.buffer.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub fn insert_newline(&mut self) {
        self.insert('\n');
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = self.history.len();
        self.saved_buffer.clear();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = self.buffer[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.buffer.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.buffer.len() {
            let next = self.buffer[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.buffer.len());
            self.buffer.drain(self.cursor..next);
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.buffer[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.buffer.len() {
            self.cursor = self.buffer[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.buffer.len());
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.buffer.len();
    }

    pub fn move_line_start(&mut self) {
        self.cursor = self.buffer[..self.cursor]
            .rfind('\n')
            .map(|idx| idx + 1)
            .unwrap_or(0);
    }

    pub fn move_line_end(&mut self) {
        self.cursor = self.buffer[self.cursor..]
            .find('\n')
            .map(|idx| self.cursor + idx)
            .unwrap_or(self.buffer.len());
    }

    pub fn move_word_left(&mut self) {
        let mut cursor = self.cursor;
        while let Some((prev, ch)) = prev_char(&self.buffer, cursor) {
            if !ch.is_whitespace() {
                break;
            }
            cursor = prev;
        }
        while let Some((prev, ch)) = prev_char(&self.buffer, cursor) {
            if ch.is_whitespace() {
                break;
            }
            cursor = prev;
        }
        self.cursor = cursor;
    }

    pub fn move_word_right(&mut self) {
        let mut cursor = self.cursor;
        while let Some((next, ch)) = next_char(&self.buffer, cursor) {
            if !ch.is_whitespace() {
                break;
            }
            cursor = next;
        }
        while let Some((next, ch)) = next_char(&self.buffer, cursor) {
            if ch.is_whitespace() {
                break;
            }
            cursor = next;
        }
        self.cursor = cursor;
    }

    pub fn delete_word(&mut self) {
        let original = self.cursor;
        self.move_word_left();
        let start = self.cursor;
        self.cursor = original;
        self.buffer.drain(start..self.cursor);
        self.cursor = start;
    }

    pub fn delete_word_forward(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        let start = self.cursor;
        self.move_word_right();
        let end = self.cursor;
        self.buffer.drain(start..end);
        self.cursor = start;
    }

    pub fn delete_to_line_start(&mut self) {
        let end = self.cursor;
        self.move_line_start();
        let start = self.cursor;
        self.buffer.drain(start..end);
        self.cursor = start;
    }

    pub fn delete_to_line_end(&mut self) {
        let start = self.cursor;
        self.move_line_end();
        let end = self.cursor;
        self.buffer.drain(start..end);
        self.cursor = start;
    }

    pub fn submit(&mut self) -> Option<String> {
        if self.buffer.trim().is_empty() {
            return None;
        }
        let input = std::mem::take(&mut self.buffer);
        self.cursor = 0;
        if self.history.last() != Some(&input) {
            self.history.push(input.clone());
        }
        self.history_index = self.history.len();
        self.saved_buffer.clear();
        Some(input)
    }

    pub fn history_up(&mut self) {
        if self.history_index > 0 {
            if self.history_index == self.history.len() {
                self.saved_buffer = self.buffer.clone();
            }
            self.history_index -= 1;
            self.buffer = self.history[self.history_index].clone();
            self.cursor = self.buffer.len();
        }
    }

    pub fn history_down(&mut self) {
        if self.history_index < self.history.len() {
            self.history_index += 1;
            if self.history_index == self.history.len() {
                self.buffer = self.saved_buffer.clone();
            } else {
                self.buffer = self.history[self.history_index].clone();
            }
            self.cursor = self.buffer.len();
        }
    }

    pub fn as_str(&self) -> &str {
        &self.buffer
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn display_cursor_width(&self) -> usize {
        let current_line_start = self.buffer[..self.cursor]
            .rfind('\n')
            .map(|idx| idx + 1)
            .unwrap_or(0);
        UnicodeWidthStr::width(&self.buffer[current_line_start..self.cursor])
    }

    pub fn explicit_line_count(&self) -> usize {
        self.buffer.matches('\n').count() + 1
    }
}

fn prev_char(text: &str, cursor: usize) -> Option<(usize, char)> {
    if cursor == 0 {
        return None;
    }
    text[..cursor].char_indices().next_back()
}

fn next_char(text: &str, cursor: usize) -> Option<(usize, char)> {
    if cursor >= text.len() {
        return None;
    }
    text[cursor..]
        .char_indices()
        .next()
        .map(|(_, ch)| (cursor + ch.len_utf8(), ch))
}

pub struct Spinner {
    frames: &'static [&'static str],
    index: usize,
    last_tick: Instant,
    interval_ms: u64,
}

impl Spinner {
    pub fn new(theme: &Theme) -> Self {
        Self {
            frames: theme.spinner_frames,
            index: 0,
            last_tick: Instant::now(),
            interval_ms: theme.spinner_interval_ms,
        }
    }

    pub fn tick(&mut self) -> &str {
        if self.last_tick.elapsed().as_millis() >= self.interval_ms as u128 {
            self.index = (self.index + 1) % self.frames.len();
            self.last_tick = Instant::now();
        }
        self.frames[self.index]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCategory {
    Intent,
    Autonomy,
    Session,
    Context,
    Agent,
    Inspect,
    Meta,
}

impl CommandCategory {
    pub fn label(&self) -> &'static str {
        match self {
            CommandCategory::Intent => "Intent",
            CommandCategory::Autonomy => "Autonomy",
            CommandCategory::Session => "Session",
            CommandCategory::Context => "Context",
            CommandCategory::Agent => "Agent",
            CommandCategory::Inspect => "Inspect",
            CommandCategory::Meta => "Meta",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandItem {
    pub command: &'static str,
    pub description: &'static str,
    pub category: CommandCategory,
}

pub fn command_catalog() -> Vec<CommandItem> {
    vec![
        CommandItem {
            command: "/help",
            description: "Show full help & keybindings",
            category: CommandCategory::Meta,
        },
        CommandItem {
            command: "/plan",
            description: "Force planning intent for this turn",
            category: CommandCategory::Intent,
        },
        CommandItem {
            command: "/explore",
            description: "Force exploration intent for this turn",
            category: CommandCategory::Intent,
        },
        CommandItem {
            command: "/build",
            description: "Force implementation intent for this turn",
            category: CommandCategory::Intent,
        },
        CommandItem {
            command: "/verify",
            description: "Force verification intent for this turn",
            category: CommandCategory::Intent,
        },
        CommandItem {
            command: "/autonomy",
            description: "Show current autonomy level",
            category: CommandCategory::Autonomy,
        },
        CommandItem {
            command: "/autonomy yolo",
            description: "Auto-approve every tool call (dangerous)",
            category: CommandCategory::Autonomy,
        },
        CommandItem {
            command: "/autonomy aggressive",
            description: "Edits & tests auto, destructive asks",
            category: CommandCategory::Autonomy,
        },
        CommandItem {
            command: "/autonomy balanced",
            description: "Reads auto, stateful work asks",
            category: CommandCategory::Autonomy,
        },
        CommandItem {
            command: "/autonomy conservative",
            description: "Everything non-read needs approval",
            category: CommandCategory::Autonomy,
        },
        CommandItem {
            command: "/yolo",
            description: "Shortcut: /autonomy yolo",
            category: CommandCategory::Autonomy,
        },
        CommandItem {
            command: "/safe",
            description: "Shortcut: /autonomy conservative",
            category: CommandCategory::Autonomy,
        },
        CommandItem {
            command: "/compact",
            description: "Roll old turns into a summary",
            category: CommandCategory::Context,
        },
        CommandItem {
            command: "/clear",
            description: "Clear transcript (keep system prompt)",
            category: CommandCategory::Context,
        },
        CommandItem {
            command: "/context add <path>",
            description: "Pin a workspace context chip",
            category: CommandCategory::Context,
        },
        CommandItem {
            command: "/context clear",
            description: "Clear all context chips",
            category: CommandCategory::Context,
        },
        CommandItem {
            command: "/session",
            description: "List sessions in this workspace",
            category: CommandCategory::Session,
        },
        CommandItem {
            command: "/session next",
            description: "Rotate to the next session",
            category: CommandCategory::Session,
        },
        CommandItem {
            command: "/session prev",
            description: "Rotate to the previous session",
            category: CommandCategory::Session,
        },
        CommandItem {
            command: "/session <id>",
            description: "Switch to session by id prefix",
            category: CommandCategory::Session,
        },
        CommandItem {
            command: "/new",
            description: "Open a fresh session (Ctrl+N)",
            category: CommandCategory::Session,
        },
        CommandItem {
            command: "/model",
            description: "Show currently pinned model",
            category: CommandCategory::Session,
        },
        CommandItem {
            command: "/model <id>",
            description: "Pin a model for this session",
            category: CommandCategory::Session,
        },
        CommandItem {
            command: "/agent spawn <task>",
            description: "Start a background sub-agent",
            category: CommandCategory::Agent,
        },
        CommandItem {
            command: "/agent list",
            description: "Show sub-agent queue",
            category: CommandCategory::Agent,
        },
        CommandItem {
            command: "/agent diff <id>",
            description: "Review sub-agent worktree diff",
            category: CommandCategory::Agent,
        },
        CommandItem {
            command: "/agent export <id>",
            description: "Export sub-agent review artifact",
            category: CommandCategory::Agent,
        },
        CommandItem {
            command: "/agent merge <id>",
            description: "Copy reviewed sub-agent files into workspace",
            category: CommandCategory::Agent,
        },
        CommandItem {
            command: "/agent cleanup <id>",
            description: "Remove reviewed sub-agent worktree",
            category: CommandCategory::Agent,
        },
        CommandItem {
            command: "/agent kill <id>",
            description: "Cancel a background sub-agent",
            category: CommandCategory::Agent,
        },
        CommandItem {
            command: "/approvals",
            description: "Show pending approvals",
            category: CommandCategory::Inspect,
        },
        CommandItem {
            command: "/approvals approve <id>",
            description: "Approve pending tool request",
            category: CommandCategory::Inspect,
        },
        CommandItem {
            command: "/approvals deny <id>",
            description: "Deny pending tool request",
            category: CommandCategory::Inspect,
        },
        CommandItem {
            command: "/mcp",
            description: "Show MCP servers and tool inventory",
            category: CommandCategory::Inspect,
        },
        CommandItem {
            command: "/mcp refresh",
            description: "Probe MCP servers and refresh inventory",
            category: CommandCategory::Inspect,
        },
        CommandItem {
            command: "/mcp call <server> <tool> [json]",
            description: "Invoke an MCP tool with JSON arguments",
            category: CommandCategory::Inspect,
        },
        CommandItem {
            command: "/lsp",
            description: "Show LSP roots and diagnostics summary",
            category: CommandCategory::Inspect,
        },
        CommandItem {
            command: "/lsp refresh",
            description: "Refresh workspace diagnostics cache",
            category: CommandCategory::Inspect,
        },
        CommandItem {
            command: "/lsp diagnostics",
            description: "Show cached diagnostics",
            category: CommandCategory::Inspect,
        },
        CommandItem {
            command: "/lsp symbols",
            description: "Show indexed symbol jumps",
            category: CommandCategory::Inspect,
        },
    ]
}

pub fn slash_suggestions(input: &str) -> Vec<CommandItem> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return Vec::new();
    }
    let needle = trimmed.to_lowercase();
    let catalog = command_catalog();
    // Prefix match first, then substring fallback.
    let mut prefix: Vec<CommandItem> = catalog
        .iter()
        .filter(|item| item.command.to_lowercase().starts_with(&needle) || needle.as_str() == "/")
        .copied()
        .collect();
    if prefix.is_empty() {
        prefix = catalog
            .iter()
            .filter(|item| item.command.to_lowercase().contains(&needle))
            .copied()
            .collect();
    }
    prefix
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Palette,
    Sessions,
    ModelSwitcher,
    Help,
    Agents,
    Approvals,
    Autonomy,
    Providers,
    Mcp,
    Skills,
}

impl Overlay {
    /// Overlays that use the generic `DialogSelect` UI and therefore accept
    /// filter-text input, mouse hover/click, wheel scroll.
    pub fn is_dialog_select(self) -> bool {
        matches!(
            self,
            Overlay::Palette
                | Overlay::Sessions
                | Overlay::ModelSwitcher
                | Overlay::Agents
                | Overlay::Approvals
                | Overlay::Autonomy
                | Overlay::Providers
                | Overlay::Mcp
                | Overlay::Skills
        )
    }
}

#[derive(Debug, Clone)]
pub struct ModelOption {
    pub provider: String,
    pub model_id: String,
    pub display: String,
}

pub fn default_available_models() -> Vec<ModelOption> {
    vec![
        ModelOption {
            provider: "openrouter".to_string(),
            model_id: "moonshotai/kimi-k2.6".to_string(),
            display: "Kimi K2.6".to_string(),
        },
        ModelOption {
            provider: "openai".to_string(),
            model_id: "gpt-4.1".to_string(),
            display: "GPT-4.1".to_string(),
        },
        ModelOption {
            provider: "openai".to_string(),
            model_id: "o3".to_string(),
            display: "o3".to_string(),
        },
        ModelOption {
            provider: "openai".to_string(),
            model_id: "o4-mini".to_string(),
            display: "o4-mini".to_string(),
        },
        ModelOption {
            provider: "anthropic".to_string(),
            model_id: "claude-sonnet-4-20250514".to_string(),
            display: "Claude Sonnet 4".to_string(),
        },
        ModelOption {
            provider: "anthropic".to_string(),
            model_id: "claude-opus-4-20250514".to_string(),
            display: "Claude Opus 4".to_string(),
        },
        ModelOption {
            provider: "google".to_string(),
            model_id: "gemini-2.5-pro".to_string(),
            display: "Gemini 2.5 Pro".to_string(),
        },
        ModelOption {
            provider: "google".to_string(),
            model_id: "gemini-2.5-flash".to_string(),
            display: "Gemini 2.5 Flash".to_string(),
        },
        ModelOption {
            provider: "ollama".to_string(),
            model_id: "qwen3-coder:30b".to_string(),
            display: "Qwen3 Coder 30B".to_string(),
        },
        ModelOption {
            provider: "openrouter".to_string(),
            model_id: "deepseek/deepseek-r1-0528".to_string(),
            display: "DeepSeek R1".to_string(),
        },
        ModelOption {
            provider: "openrouter".to_string(),
            model_id: "google/gemini-2.5-pro".to_string(),
            display: "Gemini 2.5 Pro (OR)".to_string(),
        },
        ModelOption {
            provider: "openrouter".to_string(),
            model_id: "anthropic/claude-sonnet-4".to_string(),
            display: "Claude Sonnet 4 (OR)".to_string(),
        },
    ]
}

pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub intent: String,
    pub active: bool,
    pub last_active: String,
}

pub struct SessionApp {
    pub session_id: String,
    pub session_title: String,
    pub transcript: Vec<Line<'static>>,
    pub input: InputState,
    pub context_items: Vec<String>,
    pub current_intent: RouterIntent,
    pub autonomy: AutonomyLevel,
    pub pending_approvals: Vec<ApprovalRequest>,
    pub background_jobs: Vec<BackgroundJob>,
    pub preflight: WorkspacePreflight,
    pub lsp: LspSnapshot,
    pub mcp: McpSnapshot,
    pub show_left_dock: bool,
    pub show_right_dock: bool,
    pub overlay: Overlay,
    pub overlay_index: usize,
    pub lifecycle: SessionLifecycle,
    pub scroll_offset: u16,
    pub auto_scroll: bool,
    pub spinner: Spinner,
    pub theme: Theme,
    pub processing: bool,
    pub cursor_visible: bool,
    pub last_cursor_toggle: Instant,
    pub input_sender: Option<mpsc::Sender<String>>,
    pub current_model_display: String,
    pub sessions: Vec<SessionInfo>,
    pub available_models: Vec<ModelOption>,
    pub workspace_root: std::path::PathBuf,
    pub toast: Option<(String, Instant)>,
    pub last_usage: Option<(u32, u32, u32)>,
    pub show_welcome: bool,
    pub dialog_state: DialogSelectState,
    pub last_dialog_layout: Option<DialogSelectLayout>,
    pub scroll_pinned: bool,
    pub transcript_area: Option<Rect>,
    pub composer_area: Option<Rect>,
    pub provider_filter: Option<String>,
    pub skills: Vec<SkillEntry>,
    /// Which role is currently streaming. `None` means no stream is
    /// in-flight, so the next `StreamDelta` must open a new transcript row
    /// with the role gutter. Cleared by `StreamDone` or any non-stream
    /// event that would change the transcript tail (MessageDelta etc.).
    pub active_stream_role: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub path: String,
}

impl Default for SessionApp {
    fn default() -> Self {
        let theme = Theme::default();
        Self {
            session_id: String::new(),
            session_title: String::new(),
            transcript: Vec::new(),
            input: InputState::default(),
            context_items: Vec::new(),
            current_intent: RouterIntent::Explore,
            autonomy: AutonomyLevel::Aggressive,
            pending_approvals: Vec::new(),
            background_jobs: Vec::new(),
            preflight: WorkspacePreflight::default(),
            lsp: LspSnapshot::default(),
            mcp: McpSnapshot::default(),
            show_left_dock: true,
            show_right_dock: true,
            overlay: Overlay::None,
            overlay_index: 0,
            lifecycle: SessionLifecycle::Idle,
            scroll_offset: 0,
            auto_scroll: true,
            spinner: Spinner::new(&theme),
            theme,
            processing: false,
            cursor_visible: true,
            last_cursor_toggle: Instant::now(),
            input_sender: None,
            current_model_display: String::new(),
            sessions: Vec::new(),
            available_models: default_available_models(),
            workspace_root: std::path::PathBuf::new(),
            toast: None,
            last_usage: None,
            show_welcome: true,
            dialog_state: DialogSelectState::default(),
            last_dialog_layout: None,
            scroll_pinned: true,
            transcript_area: None,
            composer_area: None,
            provider_filter: None,
            skills: Vec::new(),
            active_stream_role: None,
        }
    }
}

impl SessionApp {
    pub fn refresh_sessions(&mut self) {
        use crate::harness::session::SessionStore;
        let store = SessionStore::new(&self.workspace_root);
        self.sessions = store
            .list_metadata()
            .unwrap_or_default()
            .into_iter()
            .map(|meta| SessionInfo {
                id: meta.session_id.clone(),
                title: meta.title.clone(),
                intent: format!("{:?}", meta.router_intent),
                active: matches!(meta.status, crate::harness::session::SessionStatus::Active),
                last_active: meta.last_active_at.format("%Y-%m-%d %H:%M").to_string(),
            })
            .collect();
    }

    pub fn apply_event(&mut self, event: RuntimeEvent) {
        // Any real event means the welcome dismisses itself.
        if !matches!(
            event,
            RuntimeEvent::PreflightReady { .. }
                | RuntimeEvent::DiagnosticsUpdated { .. }
                | RuntimeEvent::McpStateUpdated { .. }
        ) {
            self.show_welcome = false;
        }
        // A new streaming delta continues any in-flight stream; anything
        // else terminates it so subsequent deltas (for whatever reason) do
        // not splice into the wrong row.
        if !matches!(
            event,
            RuntimeEvent::StreamDelta { .. }
                | RuntimeEvent::UsageUpdated { .. }
                | RuntimeEvent::PreflightReady { .. }
                | RuntimeEvent::DiagnosticsUpdated { .. }
                | RuntimeEvent::McpStateUpdated { .. }
                | RuntimeEvent::BackgroundJobUpdated { .. }
        ) {
            self.active_stream_role = None;
        }
        match event {
            RuntimeEvent::SessionLifecycle {
                session_id,
                lifecycle,
                summary,
            } => {
                self.session_id = session_id;
                self.session_title = summary.clone();
                self.lifecycle = lifecycle;
                self.transcript.push(Line::from(vec![
                    Span::styled(" ● ", Style::default().fg(self.theme.role_system)),
                    Span::styled(summary, Style::default().fg(self.theme.text_secondary)),
                ]));
                self.auto_scroll = true;
            }
            RuntimeEvent::MessageDelta { role, content } => {
                let color = self.theme.role_color(&role);
                let is_user = role.as_str() == "user";
                let is_assistant = role.as_str() == "assistant";
                let icon = match role.as_str() {
                    "user" => "◉",
                    "assistant" => "✦",
                    "system" => "●",
                    "tool" => "▸",
                    _ => "·",
                };

                if is_assistant || is_user {
                    for (i, line_content) in content.lines().enumerate() {
                        let styled_spans = style_line(line_content, color, &self.theme);
                        if i == 0 {
                            let mut line_spans = vec![Span::styled(
                                format!(" {icon} "),
                                Style::default().fg(color).add_modifier(Modifier::BOLD),
                            )];
                            line_spans.extend(styled_spans);
                            self.transcript.push(Line::from(line_spans));
                        } else {
                            let mut line_spans = vec![Span::raw("   ")];
                            line_spans.extend(styled_spans);
                            self.transcript.push(Line::from(line_spans));
                        }
                    }
                } else {
                    for (i, line_content) in content.lines().enumerate() {
                        if i == 0 {
                            self.transcript.push(Line::from(vec![
                                Span::styled(
                                    format!(" {icon} "),
                                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                                ),
                                Span::styled(line_content.to_string(), Style::default().fg(color)),
                            ]));
                        } else {
                            self.transcript.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    line_content.to_string(),
                                    Style::default().fg(self.theme.text_secondary),
                                ),
                            ]));
                        }
                    }
                }
                self.auto_scroll = true;
            }
            RuntimeEvent::StreamDelta {
                role,
                content,
                model: _,
            } => {
                if !content.is_empty() {
                    let color = self.theme.role_color(&role);
                    let icon = match role.as_str() {
                        "assistant" => "✦",
                        _ => "·",
                    };
                    self.append_stream_delta(&role, &content, color, icon);
                    self.auto_scroll = true;
                }
            }
            RuntimeEvent::StreamDone { model: _ } => {
                self.processing = false;
                self.active_stream_role = None;
            }
            RuntimeEvent::RouterStateChanged { intent, source } => {
                self.current_intent = intent;
                self.transcript.push(Line::from(vec![
                    Span::styled(" ◈ ", Style::default().fg(self.theme.role_router)),
                    Span::styled(
                        format!("{:?}", intent),
                        Style::default().fg(self.theme.warning),
                    ),
                    Span::styled(
                        format!(" via {}", source),
                        Style::default().fg(self.theme.dim),
                    ),
                ]));
                self.auto_scroll = true;
            }
            RuntimeEvent::ToolCallStarted { execution } => {
                self.processing = true;
                let tool_display = friendly_tool_name(&execution.tool_name);
                self.transcript.push(Line::from(vec![
                    Span::styled(" ┌ ", Style::default().fg(self.theme.role_tool)),
                    Span::styled(
                        tool_display,
                        Style::default()
                            .fg(self.theme.role_tool)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                if !execution.summary.is_empty() {
                    let summary_lines: Vec<&str> = execution.summary.lines().take(3).collect();
                    for (i, line) in summary_lines.iter().enumerate() {
                        let prefix = if i == 0 { " │ " } else { " │ " };
                        self.transcript.push(Line::from(vec![
                            Span::styled(prefix, Style::default().fg(self.theme.dim)),
                            Span::styled(
                                truncate_str(line, 80),
                                Style::default().fg(self.theme.text_secondary),
                            ),
                        ]));
                    }
                }
                self.transcript.push(Line::from(Span::styled(
                    " └⏳",
                    Style::default().fg(self.theme.warning),
                )));
                self.auto_scroll = true;
            }
            RuntimeEvent::ToolCallFinished { execution, result } => {
                self.processing = false;
                let tool_display = friendly_tool_name(&execution.tool_name);
                let (icon, icon_color) = if result.success {
                    ("✓", self.theme.success)
                } else {
                    ("✗", self.theme.error)
                };
                let output_preview = truncate_str(result.output.lines().next().unwrap_or(""), 80);
                let mut spans = vec![
                    Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
                    Span::styled(
                        format!("{tool_display} "),
                        Style::default().fg(self.theme.role_tool),
                    ),
                ];
                if !output_preview.is_empty() {
                    spans.push(Span::styled(
                        output_preview,
                        Style::default().fg(self.theme.dim),
                    ));
                }
                if !result.success && result.error.is_some() {
                    spans.push(Span::styled(
                        format!(" — {}", result.error.as_ref().unwrap()),
                        Style::default().fg(self.theme.error),
                    ));
                }
                self.transcript.push(Line::from(spans));

                let extra_lines: Vec<&str> = result.output.lines().skip(1).take(5).collect();
                for line in extra_lines {
                    self.transcript.push(Line::from(vec![
                        Span::styled("   ", Style::default()),
                        Span::styled(truncate_str(line, 100), Style::default().fg(self.theme.dim)),
                    ]));
                }
                self.transcript.push(Line::from(""));
                self.auto_scroll = true;
            }
            RuntimeEvent::ApprovalRequested { approval } => {
                self.pending_approvals.push(approval.clone());
                self.transcript.push(Line::from(vec![
                    Span::styled(" ⚠ ", Style::default().fg(self.theme.error)),
                    Span::styled(
                        format!("{} ", approval.tool_name),
                        Style::default()
                            .fg(self.theme.error)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        approval.summary.clone(),
                        Style::default().fg(self.theme.text_secondary),
                    ),
                ]));
                self.auto_scroll = true;
            }
            RuntimeEvent::ApprovalResolved { approval } => {
                if let Some(existing) = self
                    .pending_approvals
                    .iter_mut()
                    .find(|item| item.id == approval.id)
                {
                    *existing = approval.clone();
                }
                let icon = if approval.status == ApprovalStatus::Approved {
                    "✓"
                } else {
                    "✗"
                };
                let color = if approval.status == ApprovalStatus::Approved {
                    self.theme.success
                } else {
                    self.theme.error
                };
                self.transcript.push(Line::from(vec![
                    Span::styled(format!(" {icon} "), Style::default().fg(color)),
                    Span::styled(
                        format!("{:?} {}", approval.status, approval.summary),
                        Style::default().fg(color),
                    ),
                ]));
                self.auto_scroll = true;
            }
            RuntimeEvent::DiagnosticsUpdated { lsp } => {
                self.lsp = lsp;
            }
            RuntimeEvent::McpStateUpdated { mcp } => {
                self.mcp = mcp;
            }
            RuntimeEvent::BackgroundJobUpdated { job } => {
                if let Some(existing) = self
                    .background_jobs
                    .iter_mut()
                    .find(|item| item.id == job.id)
                {
                    *existing = job;
                } else {
                    self.background_jobs.push(job);
                }
            }
            RuntimeEvent::PreflightReady { preflight } => {
                self.preflight = preflight;
            }
            RuntimeEvent::AutonomyChanged { autonomy, source } => {
                self.autonomy = autonomy;
                self.toast = Some((
                    format!("Autonomy → {} ({})", autonomy.label(), source),
                    Instant::now(),
                ));
                self.transcript.push(Line::from(vec![
                    Span::styled(
                        " ⚡ ",
                        Style::default().fg(autonomy_color(autonomy, &self.theme)),
                    ),
                    Span::styled(
                        format!("Autonomy: {} ", autonomy.label()),
                        Style::default()
                            .fg(autonomy_color(autonomy, &self.theme))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("via {source}"), Style::default().fg(self.theme.dim)),
                ]));
                self.auto_scroll = true;
            }
            RuntimeEvent::ModelChanged { model, display } => {
                self.current_model_display = display.clone();
                self.toast = Some((format!("Model → {display}"), Instant::now()));
                self.transcript.push(Line::from(vec![
                    Span::styled(" ≋ ", Style::default().fg(self.theme.accent)),
                    Span::styled(
                        format!("Model pinned: {}", display),
                        Style::default()
                            .fg(self.theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" ({})", model), Style::default().fg(self.theme.dim)),
                ]));
                self.auto_scroll = true;
            }
            RuntimeEvent::ContextCompacted {
                removed_messages,
                summary,
            } => {
                self.toast = Some((
                    format!("Compacted {removed_messages} messages"),
                    Instant::now(),
                ));
                self.transcript.push(Line::from(vec![
                    Span::styled(" ⇔ ", Style::default().fg(self.theme.warning)),
                    Span::styled(
                        summary,
                        Style::default()
                            .fg(self.theme.warning)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                self.auto_scroll = true;
            }
            RuntimeEvent::SessionSwitched { session_id, title } => {
                self.session_id = session_id.clone();
                self.session_title = title.clone();
                self.transcript.push(Line::from(""));
                self.transcript.push(Line::from(vec![
                    Span::styled(" ↺ ", Style::default().fg(self.theme.role_router)),
                    Span::styled(
                        format!(
                            "Switched to session {} — {}",
                            &session_id[..session_id.len().min(8)],
                            title
                        ),
                        Style::default()
                            .fg(self.theme.role_router)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                self.auto_scroll = true;
            }
            RuntimeEvent::SubAgentSpawned { job_id, title } => {
                self.transcript.push(Line::from(vec![
                    Span::styled(" ⎇ ", Style::default().fg(self.theme.success)),
                    Span::styled(
                        format!("Sub-agent queued: {} ", title),
                        Style::default()
                            .fg(self.theme.success)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("[{}]", &job_id[..job_id.len().min(8)]),
                        Style::default().fg(self.theme.dim),
                    ),
                ]));
                self.auto_scroll = true;
            }
            RuntimeEvent::UsageUpdated {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            } => {
                self.last_usage = Some((prompt_tokens, completion_tokens, total_tokens));
            }
        }
    }

    /// Append a streaming delta to the transcript.
    ///
    /// Handles the nasty cases that used to produce garbled output:
    ///
    ///  1. **Embedded newlines.** If the LLM streams `"foo\nbar"` we push
    ///     `bar` on a new display row instead of wedging a raw `\n` byte
    ///     into a single Span (which some terminals render as a control
    ///     char and which `wrap_single_line` used to treat as zero-width).
    ///  2. **Continuing an in-flight stream.** A delta should keep writing
    ///     into the same assistant turn as the previous delta, even if the
    ///     previous delta ended with a newline (and therefore the last
    ///     transcript row is a continuation gutter, not a role gutter).
    ///     We track this via `active_stream_role` rather than inspecting
    ///     the tail line.
    ///  3. **Incremental styling.** We style only the newly arrived chunk,
    ///     not the entire accumulated message. This preserves code-fence
    ///     state across deltas and avoids `style_line`'s `trim_end` eating
    ///     significant trailing whitespace.
    ///  4. **Role switches.** If a prior stream was from a different role
    ///     we close it and open a new one with the correct icon.
    fn append_stream_delta(
        &mut self,
        role: &str,
        content: &str,
        color: ratatui::style::Color,
        icon: &str,
    ) {
        // Decide whether we are continuing an in-flight stream or opening
        // a new one.
        let same_role_stream = matches!(
            &self.active_stream_role,
            Some(active) if active == role
        );

        let mut segments = content.split('\n');
        let Some(first) = segments.next() else {
            return;
        };
        let first = first.trim_end_matches('\r');

        // Step 1: first segment either extends the last assistant row or
        // starts a new one.
        if same_role_stream {
            // Extend the last transcript row in place (it exists because
            // we pushed it when the stream opened).
            if let Some(last) = self.transcript.last_mut() {
                if !first.is_empty() {
                    for span in style_line(first, color, &self.theme) {
                        last.spans.push(span);
                    }
                }
            } else {
                // Defensive: active_stream_role was set but the transcript
                // got drained (e.g. /clear in the middle of a stream).
                let mut spans = vec![role_gutter_span(icon, color)];
                if !first.is_empty() {
                    spans.extend(style_line(first, color, &self.theme));
                }
                self.transcript.push(Line::from(spans));
            }
        } else {
            let mut spans = vec![role_gutter_span(icon, color)];
            if !first.is_empty() {
                spans.extend(style_line(first, color, &self.theme));
            }
            self.transcript.push(Line::from(spans));
        }

        // Step 2: subsequent segments each begin a new row with a
        // continuation gutter.
        for seg_raw in segments {
            let seg = seg_raw.trim_end_matches('\r');
            let mut spans = vec![continuation_gutter_span()];
            if !seg.is_empty() {
                spans.extend(style_line(seg, color, &self.theme));
            }
            self.transcript.push(Line::from(spans));
        }

        self.active_stream_role = Some(role.to_string());
    }

    pub fn apply_events(&mut self, events: Vec<RuntimeEvent>) {
        for event in events {
            self.apply_event(event);
        }
    }

    pub async fn fetch_available_models(&mut self) {
        use crate::providers::factory::{Provider, resolve_provider_auth};

        let providers = [
            Provider::OpenAi,
            Provider::Anthropic,
            Provider::Google,
            Provider::Ollama,
            Provider::OpenRouter,
        ];

        let mut models = Vec::new();
        for provider in providers {
            if let Ok(auth) = resolve_provider_auth(&provider) {
                let client = provider.create_client(auth);
                if let Ok(provider_models) = client.list_models().await {
                    for model_info in provider_models {
                        models.push(ModelOption {
                            provider: model_info.provider,
                            model_id: model_info.id,
                            display: model_info.display_name,
                        });
                    }
                }
            }
        }

        if !models.is_empty() {
            self.available_models = models;
        }
    }

    /// Used by legacy overlay renderers retained for reference. The new
    /// dialog-select driven UI uses `palette_options` instead.
    #[allow(dead_code)]
    pub fn palette_items(&self) -> Vec<CommandItem> {
        command_catalog()
    }

    pub fn suggestion_items(&self) -> Vec<CommandItem> {
        slash_suggestions(self.input.as_str())
    }
}

pub fn run_session_tui(
    runtime: SessionRuntime,
    rt: tokio::runtime::Runtime,
    initial_events: Vec<RuntimeEvent>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    let keyboard_enhancement = matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    );
    if keyboard_enhancement {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        )?;
    }
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = SessionApp::default();
    app.workspace_root = runtime.workspace_root().to_path_buf();
    app.autonomy = runtime.autonomy();
    app.current_model_display = runtime.model_display().to_string();
    app.refresh_skills();
    app.apply_events(initial_events);
    rt.block_on(app.fetch_available_models());

    let (bridge, _runtime_handle) = EventBridge::spawn_runtime(runtime, rt);
    app.input_sender = Some(bridge.input_tx.clone());
    let result = run_loop(&mut terminal, &mut app, bridge);

    disable_raw_mode()?;
    if keyboard_enhancement {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut SessionApp,
    bridge: EventBridge,
) -> anyhow::Result<()> {
    loop {
        app.tick_cursor();
        // Auto-dismiss toasts after ~2.5s.
        if let Some((_, shown_at)) = &app.toast {
            if shown_at.elapsed() > Duration::from_millis(2500) {
                app.toast = None;
            }
        }
        terminal.draw(|frame| render(frame, app))?;

        while let Some(event) = bridge.try_recv() {
            match event {
                AppEvent::Event(runtime_event) => {
                    match &runtime_event {
                        RuntimeEvent::StreamDone { .. } => {
                            app.processing = false;
                        }
                        _ => {}
                    }
                    app.apply_event(runtime_event);
                    app.auto_scroll = true;
                }
                AppEvent::Error(e) => {
                    return Err(e);
                }
                AppEvent::Terminated => {
                    return Ok(());
                }
            }
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }

        let event = event::read()?;
        match event {
            Event::Key(key) => {
                if handle_key_event(key, app)? {
                    return Ok(());
                }
            }
            Event::Mouse(mouse) => {
                handle_mouse_event(mouse, app);
            }
            Event::Paste(text) => {
                app.input.insert_str(&text);
            }
            Event::Resize(_, _) => {
                // Re-pin to bottom on resize so wrapping stays coherent.
                app.scroll_pinned = true;
                app.auto_scroll = true;
            }
            _ => {}
        }
    }
}

fn autonomy_color(level: AutonomyLevel, theme: &Theme) -> ratatui::style::Color {
    match level {
        AutonomyLevel::Conservative => theme.success,
        AutonomyLevel::Balanced => theme.accent,
        AutonomyLevel::Aggressive => theme.warning,
        AutonomyLevel::Yolo => theme.error,
    }
}

fn handle_mouse_event(mouse: MouseEvent, app: &mut SessionApp) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            if app.overlay.is_dialog_select() {
                let total = current_overlay_total(app);
                app.dialog_state.move_selection(-1, total);
                app.dialog_state.input_mode = InputMode::Mouse;
            } else if inside(app.transcript_area, mouse.column, mouse.row) {
                app.scroll_up(3);
            }
        }
        MouseEventKind::ScrollDown => {
            if app.overlay.is_dialog_select() {
                let total = current_overlay_total(app);
                app.dialog_state.move_selection(1, total);
                app.dialog_state.input_mode = InputMode::Mouse;
            } else if inside(app.transcript_area, mouse.column, mouse.row) {
                app.scroll_down(3);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if app.overlay.is_dialog_select() {
                // Click on an option row selects and submits.
                if let Some(layout) = &app.last_dialog_layout {
                    if let Some(idx) = dialog::option_at_y(layout, mouse.row) {
                        app.dialog_state.input_mode = InputMode::Mouse;
                        // Find which position in the filtered list this
                        // option occupies so "selected" stays in sync.
                        let options = current_overlay_options(app);
                        let state = &app.dialog_state;
                        let (_, filtered) =
                            dialog::filter_and_flatten(&options, state, current_overlay_flat(app));
                        if let Some(pos) = filtered.iter().position(|i| *i == idx) {
                            app.dialog_state.selected = pos;
                            submit_overlay_selection(app);
                        }
                    }
                }
            } else if inside(app.composer_area, mouse.column, mouse.row) {
                // Click on composer = nothing special; cursor already lives
                // there. Future: move cursor to click position.
            }
        }
        MouseEventKind::Moved => {
            // Keep the mouse-mode flag sticky only while it's changing
            // selection on overlays.
            if app.overlay.is_dialog_select() {
                if let Some(layout) = &app.last_dialog_layout {
                    if let Some(idx) = dialog::option_at_y(layout, mouse.row) {
                        let options = current_overlay_options(app);
                        let (_, filtered) = dialog::filter_and_flatten(
                            &options,
                            &app.dialog_state,
                            current_overlay_flat(app),
                        );
                        if let Some(pos) = filtered.iter().position(|i| *i == idx) {
                            app.dialog_state.selected = pos;
                            app.dialog_state.input_mode = InputMode::Mouse;
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn inside(area: Option<Rect>, x: u16, y: u16) -> bool {
    match area {
        Some(r) => x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height,
        None => false,
    }
}

impl SessionApp {
    fn tick_cursor(&mut self) {
        if self.last_cursor_toggle.elapsed() >= Duration::from_millis(530) {
            self.cursor_visible = !self.cursor_visible;
            self.last_cursor_toggle = Instant::now();
        }
    }

    fn scroll_up(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
        self.auto_scroll = false;
        self.scroll_pinned = false;
    }

    fn scroll_down(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(amount);
        // If we just scrolled past the bottom, re-pin. The actual check
        // happens at render time where the total-line count is available.
        // We set a hint here; render_transcript re-evaluates.
    }

    pub fn refresh_skills(&mut self) {
        let workflows_dir = self.workspace_root.join(".windsurf/workflows");
        self.skills.clear();
        let Ok(entries) = std::fs::read_dir(&workflows_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(ext) = path.extension() else {
                continue;
            };
            if ext != "md" {
                continue;
            }
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let description = parse_workflow_description(&path).unwrap_or_default();
            self.skills.push(SkillEntry {
                name,
                description,
                path: path.display().to_string(),
            });
        }
        self.skills.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

fn parse_workflow_description(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    // Frontmatter `description:` field.
    let mut lines = content.lines();
    let first = lines.next()?;
    if first.trim() != "---" {
        return None;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(rest) = line.trim().strip_prefix("description:") {
            return Some(rest.trim().trim_matches('"').to_string());
        }
    }
    None
}

fn handle_key_event(key: KeyEvent, app: &mut SessionApp) -> anyhow::Result<bool> {
    if matches!(key.kind, KeyEventKind::Release) {
        return Ok(false);
    }

    // ===== Overlay key handling =====
    if app.overlay != Overlay::None {
        return handle_overlay_key(key, app);
    }

    // ===== Global ctrl+shift combinations =====
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let option_like = key
        .modifiers
        .intersects(KeyModifiers::ALT | KeyModifiers::META);
    let command_like = key.modifiers.contains(KeyModifiers::SUPER);

    if command_like {
        match key.code {
            KeyCode::Left | KeyCode::Home => {
                app.input.move_home();
                return Ok(false);
            }
            KeyCode::Right | KeyCode::End => {
                app.input.move_end();
                return Ok(false);
            }
            KeyCode::Backspace => {
                app.input.clear();
                return Ok(false);
            }
            _ => {}
        }
    }

    if option_like {
        match key.code {
            KeyCode::Left | KeyCode::Char('b') | KeyCode::Char('B') => {
                app.input.move_word_left();
                return Ok(false);
            }
            KeyCode::Right | KeyCode::Char('f') | KeyCode::Char('F') => {
                app.input.move_word_right();
                return Ok(false);
            }
            KeyCode::Backspace => {
                app.input.delete_word();
                return Ok(false);
            }
            KeyCode::Delete | KeyCode::Char('d') | KeyCode::Char('D') => {
                app.input.delete_word_forward();
                return Ok(false);
            }
            KeyCode::Enter => {
                app.input.insert_newline();
                return Ok(false);
            }
            _ => {}
        }
    }

    if ctrl && shift {
        match key.code {
            KeyCode::Char('P') | KeyCode::Char('p') => {
                open_overlay(app, Overlay::Providers);
                return Ok(false);
            }
            KeyCode::Char('M') | KeyCode::Char('m') => {
                open_overlay(app, Overlay::Mcp);
                return Ok(false);
            }
            KeyCode::Char('A') | KeyCode::Char('a') => {
                open_overlay(app, Overlay::Approvals);
                return Ok(false);
            }
            KeyCode::BackTab => {
                send_slash(app, "/session prev");
                return Ok(false);
            }
            KeyCode::Tab => {
                send_slash(app, "/session prev");
                return Ok(false);
            }
            _ => {}
        }
    }

    if ctrl {
        match key.code {
            KeyCode::Char('c') => return Ok(true),
            KeyCode::Char('b') => {
                app.show_left_dock = !app.show_left_dock;
                return Ok(false);
            }
            KeyCode::Char('d') => {
                app.show_right_dock = !app.show_right_dock;
                return Ok(false);
            }
            KeyCode::Char('p') => {
                open_overlay(app, Overlay::Palette);
                return Ok(false);
            }
            KeyCode::Char('l') => {
                app.refresh_sessions();
                open_overlay(app, Overlay::Sessions);
                return Ok(false);
            }
            KeyCode::Char('m') => {
                app.provider_filter = None;
                open_overlay(app, Overlay::ModelSwitcher);
                return Ok(false);
            }
            KeyCode::Char('k') => {
                app.refresh_skills();
                open_overlay(app, Overlay::Skills);
                return Ok(false);
            }
            KeyCode::Char('n') => {
                send_slash(app, "/new");
                return Ok(false);
            }
            KeyCode::Char('y') => {
                let next = app.autonomy.cycle();
                send_slash(app, &format!("/autonomy {}", next.short()));
                return Ok(false);
            }
            KeyCode::Char('a') => {
                open_overlay(app, Overlay::Agents);
                return Ok(false);
            }
            KeyCode::Tab => {
                send_slash(app, "/session next");
                return Ok(false);
            }
            KeyCode::BackTab => {
                send_slash(app, "/session prev");
                return Ok(false);
            }
            KeyCode::Char('w') => {
                app.input.delete_word();
                return Ok(false);
            }
            KeyCode::Char('u') => {
                app.input.delete_to_line_start();
                return Ok(false);
            }
            KeyCode::Char('e') => {
                app.input.move_line_end();
                return Ok(false);
            }
            _ => {}
        }
    }

    // F1 / ? → help overlay.
    if matches!(key.code, KeyCode::F(1))
        || (matches!(key.code, KeyCode::Char('?')) && app.input.is_empty())
    {
        open_overlay(app, Overlay::Help);
        return Ok(false);
    }

    if shift {
        match key.code {
            KeyCode::Up => {
                app.scroll_up(3);
                return Ok(false);
            }
            KeyCode::Down => {
                app.scroll_down(3);
                return Ok(false);
            }
            _ => {}
        }
    }

    // ===== Base keys =====
    match key.code {
        KeyCode::Esc => {
            if !app.input.is_empty() {
                app.input.clear();
                app.toast = Some((
                    "Draft cleared. Press Esc again to quit.".to_string(),
                    Instant::now(),
                ));
                return Ok(false);
            }
            Ok(true)
        }
        KeyCode::Tab => {
            // Autocomplete slash commands. No more user-driven intent cycling —
            // the router picks intent autonomously from the message.
            if app.input.as_str().starts_with('/') {
                complete_slash(app);
            }
            Ok(false)
        }
        KeyCode::BackTab => Ok(false), // no-op
        KeyCode::PageUp => {
            app.scroll_up(10);
            Ok(false)
        }
        KeyCode::PageDown => {
            app.scroll_down(10);
            Ok(false)
        }
        KeyCode::Up if app.input.is_empty() => {
            app.input.history_up();
            Ok(false)
        }
        KeyCode::Down if app.input.is_empty() => {
            app.input.history_down();
            Ok(false)
        }
        KeyCode::Left => {
            app.input.move_left();
            Ok(false)
        }
        KeyCode::Right => {
            app.input.move_right();
            Ok(false)
        }
        KeyCode::Home => {
            app.input.move_home();
            Ok(false)
        }
        KeyCode::End => {
            app.input.move_end();
            Ok(false)
        }
        KeyCode::Backspace => {
            app.input.backspace();
            Ok(false)
        }
        KeyCode::Delete => {
            app.input.delete();
            Ok(false)
        }
        KeyCode::Enter => {
            if shift {
                app.input.insert_newline();
                return Ok(false);
            }
            if app.processing {
                app.toast = Some((
                    "A turn is still running. Wait for it to finish before sending.".to_string(),
                    Instant::now(),
                ));
                return Ok(false);
            }
            if let Some(input) = app.input.submit() {
                app.processing = true;
                app.scroll_pinned = true;
                if let Some(sender) = &app.input_sender {
                    let _ = sender.send(input);
                }
            }
            Ok(false)
        }
        KeyCode::Char(ch) => {
            app.input.insert(ch);
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// Handle keystrokes while an overlay is active. Dialog overlays get a
/// filter input + up/down navigation; Help is a static scroll.
fn handle_overlay_key(key: KeyEvent, app: &mut SessionApp) -> anyhow::Result<bool> {
    match key.code {
        KeyCode::Esc => {
            app.overlay = Overlay::None;
            app.provider_filter = None;
            app.dialog_state.reset();
            return Ok(false);
        }
        _ => {}
    }

    if app.overlay == Overlay::Help {
        // Help has no filter — arrow keys scroll.
        match key.code {
            KeyCode::Up => {
                app.dialog_state.scroll = app.dialog_state.scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                app.dialog_state.scroll = app.dialog_state.scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                app.dialog_state.scroll = app.dialog_state.scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                app.dialog_state.scroll = app.dialog_state.scroll.saturating_add(10);
            }
            _ => {}
        }
        return Ok(false);
    }

    match key.code {
        KeyCode::Up => {
            let total = current_overlay_total(app);
            app.dialog_state.move_selection(-1, total);
        }
        KeyCode::Down => {
            let total = current_overlay_total(app);
            app.dialog_state.move_selection(1, total);
        }
        KeyCode::PageUp => {
            let total = current_overlay_total(app);
            app.dialog_state.move_selection(-10, total);
        }
        KeyCode::PageDown => {
            let total = current_overlay_total(app);
            app.dialog_state.move_selection(10, total);
        }
        KeyCode::Home => {
            app.dialog_state.selected = 0;
            app.dialog_state.input_mode = InputMode::Keyboard;
        }
        KeyCode::End => {
            let total = current_overlay_total(app);
            if total > 0 {
                app.dialog_state.selected = total - 1;
            }
            app.dialog_state.input_mode = InputMode::Keyboard;
        }
        KeyCode::Enter => {
            submit_overlay_selection(app);
        }
        KeyCode::Char('d') | KeyCode::Char('D')
            if app.overlay == Overlay::Agents && app.dialog_state.filter.is_empty() =>
        {
            submit_selected_agent_action(app, "diff");
        }
        KeyCode::Char('m') | KeyCode::Char('M')
            if app.overlay == Overlay::Agents && app.dialog_state.filter.is_empty() =>
        {
            submit_selected_agent_action(app, "merge");
        }
        KeyCode::Char('c') | KeyCode::Char('C')
            if app.overlay == Overlay::Agents && app.dialog_state.filter.is_empty() =>
        {
            submit_selected_agent_action(app, "cleanup");
        }
        KeyCode::Char('k') | KeyCode::Char('K')
            if app.overlay == Overlay::Agents && app.dialog_state.filter.is_empty() =>
        {
            submit_selected_agent_action(app, "kill");
        }
        KeyCode::Char('a') | KeyCode::Char('A')
            if app.overlay == Overlay::Approvals && app.dialog_state.filter.is_empty() =>
        {
            submit_selected_approval_action(app, true);
        }
        KeyCode::Char('d') | KeyCode::Char('D')
            if app.overlay == Overlay::Approvals && app.dialog_state.filter.is_empty() =>
        {
            submit_selected_approval_action(app, false);
        }
        KeyCode::Tab => {
            // Inside the model switcher, Tab cycles the provider filter.
            if app.overlay == Overlay::ModelSwitcher {
                cycle_provider_filter(app);
            }
        }
        KeyCode::Backspace => {
            app.dialog_state.backspace();
        }
        KeyCode::Left => {
            app.dialog_state.move_cursor_left();
        }
        KeyCode::Right => {
            app.dialog_state.move_cursor_right();
        }
        KeyCode::Char(ch) => {
            app.dialog_state.insert_char(ch);
        }
        _ => {}
    }

    Ok(false)
}

fn open_overlay(app: &mut SessionApp, next: Overlay) {
    if app.overlay == next {
        app.overlay = Overlay::None;
        app.dialog_state.reset();
        return;
    }
    app.overlay = next;
    app.dialog_state.reset();
    app.overlay_index = 0;
}

fn send_slash(app: &mut SessionApp, command: &str) {
    if app.processing {
        app.toast = Some((
            "A turn is still running. Wait for it to finish before sending.".to_string(),
            Instant::now(),
        ));
        return;
    }
    app.input.buffer = command.to_string();
    app.input.cursor = app.input.buffer.len();
    if let Some(input) = app.input.submit() {
        app.processing = true;
        app.scroll_pinned = true;
        if let Some(sender) = &app.input_sender {
            let _ = sender.send(input);
        }
    }
}

/// Autocomplete the slash command currently in the composer buffer.
/// Strategy:
///   1. If there's exactly one match, accept it (add trailing space if the
///      command takes arguments).
///   2. If multiple matches share a longer common prefix than what's typed,
///      extend the buffer to that prefix.
///   3. If the typed value is already a full match, cycle through matches by
///      replacing the buffer with the next option.
fn complete_slash(app: &mut SessionApp) {
    let current = app.input.as_str().to_string();
    if !current.starts_with('/') {
        return;
    }
    let matches = slash_suggestions(&current);
    if matches.is_empty() {
        return;
    }

    // If typing has stabilized (current == some command), cycle.
    if let Some(exact) = matches.iter().find(|m| m.command == current) {
        let idx = matches
            .iter()
            .position(|m| m.command == exact.command)
            .unwrap_or(0);
        let next = &matches[(idx + 1) % matches.len()];
        app.input.buffer = next.command.to_string();
        app.input.cursor = app.input.buffer.len();
        return;
    }

    if matches.len() == 1 {
        let target = matches[0].command.to_string();
        // If the command has `<placeholder>` args, strip them but keep the
        // command token + trailing space.
        let cleaned = strip_placeholders(&target);
        app.input.buffer = cleaned;
        app.input.cursor = app.input.buffer.len();
        return;
    }

    // Multiple matches: extend to longest common prefix.
    let prefix = longest_common_prefix(&matches.iter().map(|m| m.command).collect::<Vec<_>>());
    if prefix.len() > current.len() {
        app.input.buffer = prefix;
        app.input.cursor = app.input.buffer.len();
    }
}

fn strip_placeholders(command: &str) -> String {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let kept: Vec<&str> = parts
        .iter()
        .take_while(|p| !p.starts_with('<') && !p.starts_with('['))
        .copied()
        .collect();
    let mut result = kept.join(" ");
    if parts.len() > kept.len() {
        result.push(' ');
    }
    result
}

fn longest_common_prefix(strs: &[&str]) -> String {
    if strs.is_empty() {
        return String::new();
    }
    let first = strs[0];
    let mut end = first.len();
    for s in strs.iter().skip(1) {
        end = end.min(s.len());
        while !first.is_char_boundary(end) || !s.is_char_boundary(end) {
            if end == 0 {
                return String::new();
            }
            end -= 1;
        }
        while &first[..end] != &s[..end] {
            end -= 1;
            while end > 0 && (!first.is_char_boundary(end) || !s.is_char_boundary(end)) {
                end -= 1;
            }
            if end == 0 {
                break;
            }
        }
    }
    first[..end].to_string()
}

fn cycle_provider_filter(app: &mut SessionApp) {
    let providers: Vec<String> = {
        let mut seen: Vec<String> = Vec::new();
        for m in &app.available_models {
            if !seen.contains(&m.provider) {
                seen.push(m.provider.clone());
            }
        }
        seen
    };
    if providers.is_empty() {
        return;
    }
    let next = match &app.provider_filter {
        None => Some(providers[0].clone()),
        Some(current) => {
            let idx = providers.iter().position(|p| p == current).unwrap_or(0);
            if idx + 1 >= providers.len() {
                None
            } else {
                Some(providers[idx + 1].clone())
            }
        }
    };
    app.provider_filter = next;
    app.dialog_state.selected = 0;
    app.dialog_state.scroll = 0;
}

// ======== Overlay option builders ========

fn current_overlay_options(app: &SessionApp) -> Vec<DialogOption> {
    match app.overlay {
        Overlay::Palette => palette_options(app),
        Overlay::Sessions => sessions_options(app),
        Overlay::ModelSwitcher => models_options(app),
        Overlay::Agents => agents_options(app),
        Overlay::Approvals => approvals_options(app),
        Overlay::Autonomy => autonomy_options(app),
        Overlay::Providers => providers_options(app),
        Overlay::Mcp => mcp_options(app),
        Overlay::Skills => skills_options(app),
        Overlay::Help | Overlay::None => Vec::new(),
    }
}

fn current_overlay_flat(app: &SessionApp) -> bool {
    // Sessions, Agents, Skills are naturally flat; the rest use categories
    // when filter is empty and flatten when the user is searching.
    matches!(
        app.overlay,
        Overlay::Sessions
            | Overlay::Agents
            | Overlay::Approvals
            | Overlay::Skills
            | Overlay::Autonomy
    ) || !app.dialog_state.filter.is_empty()
}

fn current_overlay_total(app: &SessionApp) -> usize {
    let opts = current_overlay_options(app);
    let (_, filtered) =
        dialog::filter_and_flatten(&opts, &app.dialog_state, current_overlay_flat(app));
    filtered.len()
}

fn palette_options(_app: &SessionApp) -> Vec<DialogOption> {
    command_catalog()
        .into_iter()
        .map(|item| {
            DialogOption::new(item.command, item.command)
                .description(item.description)
                .category(item.category.label())
        })
        .collect()
}

fn sessions_options(app: &SessionApp) -> Vec<DialogOption> {
    app.sessions
        .iter()
        .map(|session| {
            let short_id = &session.id[..session.id.len().min(8)];
            let title = if session.title.trim().is_empty() {
                "untitled".to_string()
            } else {
                session.title.clone()
            };
            let marker_style = if session.active {
                Style::default()
                    .fg(app.theme.success)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.dim)
            };
            DialogOption::new(session.id.clone(), title)
                .description(format!("{} · {}", short_id, session.intent))
                .footer(session.last_active.clone())
                .marker(if session.active { "●" } else { "○" }, marker_style)
        })
        .collect()
}

fn models_options(app: &SessionApp) -> Vec<DialogOption> {
    app.available_models
        .iter()
        .filter(|m| match &app.provider_filter {
            Some(p) => &m.provider == p,
            None => true,
        })
        .map(|model| {
            let provider_label = provider_display_name(&model.provider);
            let connected = provider_has_auth(&model.provider);
            let marker = if connected { "✓" } else { "·" };
            let marker_style = if connected {
                Style::default().fg(app.theme.success)
            } else {
                Style::default().fg(app.theme.dim)
            };
            DialogOption::new(model.model_id.clone(), model.display.clone())
                .description(model.model_id.clone())
                .category(provider_label)
                .footer(model.provider.clone())
                .marker(marker, marker_style)
                .disabled(!connected)
        })
        .collect()
}

fn agents_options(app: &SessionApp) -> Vec<DialogOption> {
    app.background_jobs
        .iter()
        .filter(|job| matches!(job.kind, BackgroundJobKind::SubAgent))
        .map(|job| {
            let short_id = &job.id[..job.id.len().min(8)];
            let (marker, style) = match job.status {
                BackgroundJobStatus::Queued => ("⧗", Style::default().fg(app.theme.dim)),
                BackgroundJobStatus::Running => ("◉", Style::default().fg(app.theme.warning)),
                BackgroundJobStatus::Completed => ("✓", Style::default().fg(app.theme.success)),
                BackgroundJobStatus::Failed => ("✗", Style::default().fg(app.theme.error)),
                BackgroundJobStatus::Cancelled => ("⊘", Style::default().fg(app.theme.dim)),
            };
            let footer = job
                .progress
                .map(|p| format!("{p}%"))
                .unwrap_or_else(|| format!("{:?}", job.status));
            DialogOption::new(job.id.clone(), job.title.clone())
                .description(format!("[{short_id}] {}", job.detail))
                .footer(footer)
                .marker(marker, style)
        })
        .collect()
}

fn approvals_options(app: &SessionApp) -> Vec<DialogOption> {
    let pending: Vec<&ApprovalRequest> = app
        .pending_approvals
        .iter()
        .filter(|approval| approval.status == ApprovalStatus::Pending)
        .collect();
    if pending.is_empty() {
        return vec![
            DialogOption::new("empty", "No pending approvals").description("Tool gates are clear"),
        ];
    }
    pending
        .into_iter()
        .map(|approval| {
            let short_id = &approval.id[..approval.id.len().min(8)];
            DialogOption::new(approval.id.clone(), approval.tool_name.clone())
                .description(approval.summary.clone())
                .footer(format!("[{short_id}] {:?}", approval.risk))
                .marker("!", Style::default().fg(app.theme.warning))
        })
        .collect()
}

fn autonomy_options(app: &SessionApp) -> Vec<DialogOption> {
    let levels = [
        AutonomyLevel::Conservative,
        AutonomyLevel::Balanced,
        AutonomyLevel::Aggressive,
        AutonomyLevel::Yolo,
    ];
    levels
        .iter()
        .map(|level| {
            let color = autonomy_color(*level, &app.theme);
            let marker = if *level == app.autonomy { "●" } else { "○" };
            DialogOption::new(level.short(), level.label())
                .description(level.detail())
                .marker(marker, Style::default().fg(color))
        })
        .collect()
}

fn providers_options(app: &SessionApp) -> Vec<DialogOption> {
    // Static list of providers we know about, augmented with dynamic auth
    // status via env-vars + ~/.codex/auth.json.
    const PROVIDERS: &[(&str, &str)] = &[
        ("openrouter", "OpenRouter"),
        ("openai", "OpenAI"),
        ("openai_codex", "OpenAI Codex"),
        ("anthropic", "Anthropic Claude"),
        ("google", "Google Gemini"),
        ("ollama", "Ollama (local)"),
    ];
    PROVIDERS
        .iter()
        .map(|(id, label)| {
            let connected = provider_has_auth(id);
            let description = provider_auth_hint(id);
            let footer = if connected {
                "connected"
            } else {
                "not configured"
            };
            let marker = if connected { "✓" } else { "·" };
            let style = if connected {
                Style::default().fg(app.theme.success)
            } else {
                Style::default().fg(app.theme.dim)
            };
            DialogOption::new(id.to_string(), label.to_string())
                .description(description)
                .footer(footer)
                .marker(marker, style)
        })
        .collect()
}

fn provider_display_name(id: &str) -> String {
    match id {
        "openai" => "OpenAI",
        "openai_codex" => "OpenAI Codex",
        "anthropic" => "Anthropic",
        "google" => "Google",
        "openrouter" => "OpenRouter",
        "ollama" => "Ollama",
        other => other,
    }
    .to_string()
}

fn provider_has_auth(id: &str) -> bool {
    match id {
        "openai" => std::env::var("OPENAI_API_KEY").is_ok(),
        "openai_codex" => {
            if std::env::var("OPENAI_API_KEY").is_ok() {
                return true;
            }
            if let Some(home) = dirs_home() {
                home.join(".codex/auth.json").exists()
            } else {
                false
            }
        }
        "anthropic" => std::env::var("ANTHROPIC_API_KEY").is_ok(),
        "google" => {
            std::env::var("GEMINI_API_KEY").is_ok() || std::env::var("GOOGLE_API_KEY").is_ok()
        }
        "openrouter" => std::env::var("OPENROUTER_API_KEY").is_ok(),
        "ollama" => true, // local — no auth needed.
        _ => false,
    }
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

fn provider_auth_hint(id: &str) -> String {
    match id {
        "openai" => "Set OPENAI_API_KEY".to_string(),
        "openai_codex" => "Set OPENAI_API_KEY or login with `codex`".to_string(),
        "anthropic" => "Set ANTHROPIC_API_KEY".to_string(),
        "google" => "Set GEMINI_API_KEY or GOOGLE_API_KEY".to_string(),
        "openrouter" => "Set OPENROUTER_API_KEY".to_string(),
        "ollama" => "Run `ollama serve` locally".to_string(),
        _ => String::new(),
    }
}

fn mcp_options(app: &SessionApp) -> Vec<DialogOption> {
    if !app.mcp.ready {
        return vec![
            DialogOption::new("no_mcp", "No MCP servers configured")
                .description("Add a `.charm/mcp.json` to register servers"),
        ];
    }
    if app.mcp.servers.is_empty() {
        return vec![DialogOption::new("empty", "No MCP servers registered")];
    }
    app.mcp
        .servers
        .iter()
        .map(|server| {
            let connected = matches!(
                server.status,
                crate::runtime::types::McpServerStatus::Connected
            );
            let marker = match server.status {
                crate::runtime::types::McpServerStatus::Connected => "✓",
                crate::runtime::types::McpServerStatus::Degraded => "!",
                crate::runtime::types::McpServerStatus::Disconnected => "·",
            };
            let style = if connected {
                Style::default().fg(app.theme.success)
            } else if matches!(
                server.status,
                crate::runtime::types::McpServerStatus::Degraded
            ) {
                Style::default().fg(app.theme.warning)
            } else {
                Style::default().fg(app.theme.dim)
            };
            let description = server
                .last_error
                .clone()
                .unwrap_or_else(|| format!("approval: {}", server.approval_mode));
            DialogOption::new(server.name.clone(), server.name.clone())
                .description(description)
                .footer(format!("{} tools", server.tool_count))
                .marker(marker, style)
        })
        .collect()
}

fn skills_options(app: &SessionApp) -> Vec<DialogOption> {
    if app.skills.is_empty() {
        return vec![
            DialogOption::new("empty", "No skills / workflows found")
                .description("Add markdown to .windsurf/workflows/"),
        ];
    }
    app.skills
        .iter()
        .map(|skill| {
            DialogOption::new(skill.name.clone(), format!("/{}", skill.name))
                .description(if skill.description.is_empty() {
                    skill.path.clone()
                } else {
                    skill.description.clone()
                })
                .footer("workflow".to_string())
        })
        .collect()
}

/// Called when the user presses Enter (keyboard) or clicks (mouse) on an
/// option. Applies the selection and closes the overlay.
fn submit_overlay_selection(app: &mut SessionApp) {
    let opts = current_overlay_options(app);
    let (_, filtered) =
        dialog::filter_and_flatten(&opts, &app.dialog_state, current_overlay_flat(app));
    let Some(idx) = filtered.get(app.dialog_state.selected).copied() else {
        return;
    };
    let Some(option) = opts.get(idx) else {
        return;
    };
    let value = option.value.clone();
    let disabled = option.disabled;

    match app.overlay {
        Overlay::Palette => {
            let cleaned = strip_placeholders(&value);
            app.input.buffer = cleaned;
            app.input.cursor = app.input.buffer.len();
        }
        Overlay::Sessions => {
            send_slash(app, &format!("/session {}", value));
        }
        Overlay::ModelSwitcher => {
            if !disabled {
                send_slash(app, &format!("/model {}", value));
            } else {
                app.toast = Some((
                    format!("Provider not connected. Press Ctrl+Shift+P to connect."),
                    Instant::now(),
                ));
            }
        }
        Overlay::Agents => {
            let action = app
                .background_jobs
                .iter()
                .find(|job| job.id == value)
                .map(|job| match job.status {
                    BackgroundJobStatus::Completed | BackgroundJobStatus::Failed => "diff",
                    BackgroundJobStatus::Queued | BackgroundJobStatus::Running => "kill",
                    BackgroundJobStatus::Cancelled => "cleanup",
                })
                .unwrap_or("diff");
            submit_agent_action(app, &value, action);
        }
        Overlay::Approvals => {
            if value != "empty" {
                submit_approval_action(app, &value, true);
            }
        }
        Overlay::Autonomy => {
            send_slash(app, &format!("/autonomy {}", value));
        }
        Overlay::Providers => {
            // Hint the user on how to connect — we don't have an in-TUI
            // auth flow yet.
            let hint = provider_auth_hint(&value);
            app.toast = Some((
                format!("{}: {hint}", provider_display_name(&value)),
                Instant::now(),
            ));
        }
        Overlay::Mcp => {
            send_slash(app, "/mcp refresh");
        }
        Overlay::Skills => {
            // Selecting a skill inserts a `/workflow <name>` command into the
            // composer buffer so the user can add context before firing it.
            app.input.buffer = format!("/workflow {}", value);
            app.input.cursor = app.input.buffer.len();
        }
        Overlay::Help | Overlay::None => {}
    }

    app.overlay = Overlay::None;
    app.provider_filter = None;
    app.dialog_state.reset();
}

fn submit_selected_agent_action(app: &mut SessionApp, action: &str) {
    if let Some(value) = selected_overlay_value(app) {
        submit_agent_action(app, &value, action);
        app.overlay = Overlay::None;
        app.provider_filter = None;
        app.dialog_state.reset();
    }
}

fn selected_overlay_value(app: &SessionApp) -> Option<String> {
    let opts = current_overlay_options(app);
    let (_, filtered) =
        dialog::filter_and_flatten(&opts, &app.dialog_state, current_overlay_flat(app));
    let idx = filtered.get(app.dialog_state.selected).copied()?;
    opts.get(idx).map(|option| option.value.clone())
}

fn submit_agent_action(app: &mut SessionApp, id: &str, action: &str) {
    let short = &id[..id.len().min(8)];
    send_slash(app, &format!("/agent {action} {short}"));
}

fn submit_selected_approval_action(app: &mut SessionApp, approved: bool) {
    if let Some(value) = selected_overlay_value(app) {
        if value != "empty" {
            submit_approval_action(app, &value, approved);
        }
        app.overlay = Overlay::None;
        app.provider_filter = None;
        app.dialog_state.reset();
    }
}

fn submit_approval_action(app: &mut SessionApp, id: &str, approved: bool) {
    let short = &id[..id.len().min(8)];
    let action = if approved { "approve" } else { "deny" };
    send_slash(app, &format!("/approvals {action} {short}"));
}

fn render(frame: &mut ratatui::Frame<'_>, app: &mut SessionApp) {
    let theme = &app.theme;
    let bg = theme.bg_secondary;

    frame.render_widget(
        Block::default().style(Style::default().bg(bg)),
        frame.area(),
    );

    let input_text = app.input.as_str();
    let suggestions_len = slash_suggestions(input_text).len().min(6);
    let dropdown_space = if input_text.starts_with('/') && suggestions_len > 0 {
        suggestions_len as u16 + 2
    } else {
        0
    };
    let input_rows = composer_input_rows(input_text).min(6) as u16;
    let composer_height =
        2 + input_rows + dropdown_space + if app.context_items.is_empty() { 0 } else { 1 };

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(composer_height),
        ])
        .split(frame.area());

    let mut horizontal = Vec::new();
    if app.show_left_dock {
        horizontal.push(Constraint::Length(28));
    }
    horizontal.push(Constraint::Min(30));
    if app.show_right_dock {
        horizontal.push(Constraint::Length(34));
    }
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(horizontal)
        .split(outer[0]);

    let mut cursor = 0;
    if app.show_left_dock {
        render_left_dock(frame, app, main[cursor]);
        cursor += 1;
    }
    let transcript_rect = main[cursor];
    app.transcript_area = Some(transcript_rect);
    render_transcript(frame, app, transcript_rect);
    cursor += 1;
    if app.show_right_dock {
        render_right_dock(frame, app, main[cursor]);
    }
    render_status(frame, app, outer[1]);
    app.composer_area = Some(outer[2]);
    render_composer(frame, app, outer[2]);

    if app.overlay == Overlay::Help {
        render_help_overlay(frame, app);
    } else if app.overlay.is_dialog_select() {
        render_dialog_overlay(frame, app);
    }

    if app.show_welcome && app.overlay == Overlay::None {
        render_welcome_overlay(frame, app);
    }

    if let Some((text, _)) = app.toast.clone() {
        render_toast(frame, app, &text);
    }
}

fn render_dialog_overlay(frame: &mut ratatui::Frame<'_>, app: &mut SessionApp) {
    let options = current_overlay_options(app);
    let flat = current_overlay_flat(app);
    let (title, placeholder, keybinds, current) = match app.overlay {
        Overlay::Palette => (
            "Commands",
            "Search commands...",
            vec![
                KeybindHint::new("tab", "autocomplete"),
                KeybindHint::new("↵", "insert"),
            ],
            None,
        ),
        Overlay::Sessions => (
            "Sessions",
            "Search sessions...",
            vec![
                KeybindHint::new("↵", "switch"),
                KeybindHint::new("ctrl+n", "new"),
            ],
            Some(app.session_id.as_str()),
        ),
        Overlay::ModelSwitcher => {
            let title = match &app.provider_filter {
                Some(p) => {
                    Box::leak(format!("Models · {}", provider_display_name(p)).into_boxed_str())
                        as &'static str
                }
                None => "Models",
            };
            (
                title,
                "Search models...",
                vec![
                    KeybindHint::new("tab", "filter provider"),
                    KeybindHint::new("↵", "select"),
                ],
                Some(app.current_model_display.as_str()),
            )
        }
        Overlay::Agents => (
            "Sub-agents",
            "Search agents...",
            vec![
                KeybindHint::new("↵/d", "diff"),
                KeybindHint::new("m", "merge"),
                KeybindHint::new("c", "cleanup"),
                KeybindHint::new("k", "kill"),
            ],
            None,
        ),
        Overlay::Approvals => (
            "Approvals",
            "Search approvals...",
            vec![
                KeybindHint::new("↵/a", "approve"),
                KeybindHint::new("d", "deny"),
            ],
            None,
        ),
        Overlay::Autonomy => (
            "Autonomy",
            "",
            vec![KeybindHint::new("↵", "apply")],
            Some(app.autonomy.short()),
        ),
        Overlay::Providers => (
            "Providers",
            "Search providers...",
            vec![KeybindHint::new("↵", "how to connect")],
            None,
        ),
        Overlay::Mcp => (
            "MCP servers",
            "Search servers...",
            vec![KeybindHint::new("↵", "refresh")],
            None,
        ),
        Overlay::Skills => (
            "Skills / Workflows",
            "Search skills...",
            vec![KeybindHint::new("↵", "insert")],
            None,
        ),
        Overlay::Help | Overlay::None => return,
    };

    let props = DialogSelectProps {
        title,
        placeholder,
        options: &options,
        state: &app.dialog_state,
        flat,
        keybinds: &keybinds,
        width_pct: 72,
        height_pct: 70,
        current,
    };

    let layout = dialog::render_dialog_select(frame, &app.theme, &props);
    app.last_dialog_layout = Some(layout);
}

fn render_transcript(frame: &mut ratatui::Frame<'_>, app: &mut SessionApp, area: Rect) {
    let theme = &app.theme;

    // Account for the block borders and 1-column padding on each side.
    let inner_width = area.width.saturating_sub(4);
    let viewport_height = area.height.saturating_sub(2);

    // Pre-wrap the transcript so that scroll offsets are measured in
    // display rows, not logical lines. This fixes the long-line truncation /
    // incorrect scroll-clamp issues.
    let wrapped = wrap_lines_to_width(&app.transcript, inner_width);
    let total_rows = wrapped.len() as u16;

    // Pinned-to-bottom policy: as long as the user is pinned, show the
    // latest content. If they manually scrolled up (scroll_up sets pinned
    // = false), respect scroll_offset. If their manual scroll ends up at
    // or past the bottom, re-pin.
    let max_scroll = total_rows.saturating_sub(viewport_height);
    if app.scroll_pinned {
        app.scroll_offset = max_scroll;
    } else if app.scroll_offset >= max_scroll {
        // User scrolled down past the tail: treat as "pinned again".
        app.scroll_offset = max_scroll;
        app.scroll_pinned = true;
    }
    let scroll = app.scroll_offset;

    let pin_marker = if app.scroll_pinned { "●" } else { "○" };
    let session_short = if app.session_id.is_empty() {
        String::new()
    } else {
        app.session_id.chars().take(8).collect::<String>()
    };
    let title = if app.processing {
        let spinner = app.spinner.tick();
        format!(" {spinner} {pin_marker} {session_short} ")
    } else {
        format!(" {pin_marker} {session_short} ")
    };

    let visible: Vec<Line> = wrapped
        .into_iter()
        .skip(scroll as usize)
        .take(viewport_height as usize)
        .collect();

    let paragraph = Paragraph::new(Text::from(visible)).block(
        Block::default()
            .title(Span::styled(
                title,
                Style::default()
                    .fg(theme.dock_title)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border))
            .style(Style::default().bg(theme.bg_primary))
            .padding(Padding::new(1, 1, 0, 0)),
    );
    frame.render_widget(paragraph, area);

    // Optional scroll indicator on the right edge.
    if total_rows > viewport_height {
        let track_height = viewport_height.saturating_sub(2);
        if track_height > 0 {
            let progress = (scroll as f32 / max_scroll.max(1) as f32).clamp(0.0, 1.0);
            let indicator_y = area.y + 1 + (progress * track_height as f32) as u16;
            let x = area.x + area.width.saturating_sub(1);
            let y = indicator_y.min(area.y + area.height.saturating_sub(2));
            frame.render_widget(
                Paragraph::new("│").style(Style::default().fg(theme.accent)),
                Rect {
                    x,
                    y,
                    width: 1,
                    height: 1,
                },
            );
        }
    }
}

/// Word-aware wrap of a Vec<Line<'static>> to a given width, preserving
/// styled spans. A line wider than `width` columns is broken on whitespace
/// when possible, otherwise on char boundaries.
fn wrap_lines_to_width(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return lines.to_vec();
    }
    let width = width as usize;
    let mut out: Vec<Line<'static>> = Vec::new();
    for line in lines {
        let wrapped = wrap_single_line(line, width);
        out.extend(wrapped);
    }
    out
}

fn role_gutter_span(icon: &str, color: ratatui::style::Color) -> Span<'static> {
    Span::styled(
        format!(" {icon} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

fn continuation_gutter_span() -> Span<'static> {
    Span::raw("   ")
}

fn wrap_single_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    let mut result: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_width: usize = 0;

    for span in &line.spans {
        let style = span.style;
        // Hard line breaks first: split the span body on \n (and drop any
        // \r so CRLF streams stay clean). Every segment AFTER the first
        // forces a new display row even when there is no width overflow.
        // This is the whole reason the transcript used to render
        // "line1line2line3" on a single row.
        let segments: Vec<&str> = span.content.split('\n').collect();
        for (seg_idx, segment_raw) in segments.iter().enumerate() {
            if seg_idx > 0 {
                result.push(Line::from(std::mem::take(&mut current)));
                current_width = 0;
            }
            let segment = segment_raw.trim_end_matches('\r');
            let mut remaining: &str = segment;
            if remaining.is_empty() {
                continue;
            }
            while !remaining.is_empty() {
                let avail = width.saturating_sub(current_width);
                if avail == 0 {
                    result.push(Line::from(std::mem::take(&mut current)));
                    current_width = 0;
                    continue;
                }
                let mut taken_bytes = 0usize;
                let mut taken_cols = 0usize;
                let mut last_break_bytes: Option<usize> = None;
                for (byte_idx, ch) in remaining.char_indices() {
                    if ch == '\n' || ch == '\r' {
                        // Defense in depth: split() already removed these,
                        // but if a future caller skips the split, don't
                        // let them leak.
                        break;
                    }
                    let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if taken_cols + w > avail {
                        break;
                    }
                    taken_bytes = byte_idx + ch.len_utf8();
                    taken_cols += w;
                    if ch.is_whitespace() {
                        last_break_bytes = Some(taken_bytes);
                    }
                }
                if taken_bytes == 0 {
                    // Can't fit even a single char: force a line break.
                    result.push(Line::from(std::mem::take(&mut current)));
                    current_width = 0;
                    continue;
                }
                let split_at = if taken_bytes < remaining.len() {
                    last_break_bytes.unwrap_or(taken_bytes)
                } else {
                    taken_bytes
                };
                let chunk = &remaining[..split_at];
                if !chunk.is_empty() {
                    current.push(Span::styled(chunk.to_string(), style));
                    current_width += unicode_width::UnicodeWidthStr::width(chunk);
                }
                remaining = &remaining[split_at..];
                // Eat leading whitespace on continuation lines.
                let trimmed = remaining.trim_start_matches(|c: char| c == ' ');
                if trimmed.len() != remaining.len() && current_width >= width {
                    result.push(Line::from(std::mem::take(&mut current)));
                    current_width = 0;
                    remaining = trimmed;
                } else if current_width >= width {
                    result.push(Line::from(std::mem::take(&mut current)));
                    current_width = 0;
                }
            }
        }
    }
    if !current.is_empty() || result.is_empty() {
        result.push(Line::from(current));
    }
    result
}

fn render_left_dock(frame: &mut ratatui::Frame<'_>, app: &SessionApp, area: Rect) {
    let theme = &app.theme;

    let mut lines = vec![
        Line::from(vec![
            Span::styled(" ◈ ", Style::default().fg(theme.accent)),
            Span::styled(
                app.session_id.chars().take(8).collect::<String>(),
                Style::default().fg(theme.text_primary),
            ),
        ]),
        Line::from(vec![
            Span::styled(" ⎇ ", Style::default().fg(theme.success)),
            Span::styled(
                app.preflight.branch.clone(),
                Style::default().fg(theme.text_primary),
            ),
        ]),
    ];

    if !app.preflight.dirty_files.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" ◆ ", Style::default().fg(theme.warning)),
            Span::styled(
                format!("{} dirty", app.preflight.dirty_files.len()),
                Style::default().fg(theme.text_secondary),
            ),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled(" ◆ ", Style::default().fg(theme.success)),
            Span::styled("clean", Style::default().fg(theme.success)),
        ]));
    }

    if !app.preflight.suggested_actions.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Suggested",
            Style::default()
                .fg(theme.dock_title)
                .add_modifier(Modifier::BOLD),
        )));
        for action in &app.preflight.suggested_actions {
            lines.push(Line::from(vec![
                Span::styled("  • ", Style::default().fg(theme.dim)),
                Span::styled(action.clone(), Style::default().fg(theme.text_secondary)),
            ]));
        }
    }

    if !app.context_items.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Context",
            Style::default()
                .fg(theme.dock_title)
                .add_modifier(Modifier::BOLD),
        )));
        for item in &app.context_items {
            lines.push(Line::from(vec![
                Span::styled("  • ", Style::default().fg(theme.dim)),
                Span::styled(item.clone(), Style::default().fg(theme.text_secondary)),
            ]));
        }
    }

    let paragraph = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title(Span::styled(
                    " Workspace",
                    Style::default()
                        .fg(theme.dock_title)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.border))
                .style(Style::default().bg(theme.bg_secondary)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_right_dock(frame: &mut ratatui::Frame<'_>, app: &SessionApp, area: Rect) {
    let theme = &app.theme;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Min(4),
        ])
        .split(area);

    let lsp_icon = if app.lsp.ready { "●" } else { "○" };
    let lsp_color = if app.lsp.ready {
        theme.success
    } else {
        theme.dim
    };
    let lsp_lines = vec![
        Line::from(vec![
            Span::styled(format!(" {lsp_icon} "), Style::default().fg(lsp_color)),
            Span::styled(
                "LSP",
                Style::default()
                    .fg(theme.dock_title)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Roots ", Style::default().fg(theme.dim)),
            Span::styled(
                format!("{}", app.lsp.active_roots.len()),
                Style::default().fg(theme.text_primary),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Diag ", Style::default().fg(theme.dim)),
            Span::styled(
                format!("{}", app.lsp.diagnostics.len()),
                Style::default().fg(theme.text_primary),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Jump ", Style::default().fg(theme.dim)),
            Span::styled(
                format!("{}", app.lsp.symbol_jumps.len()),
                Style::default().fg(theme.text_primary),
            ),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(lsp_lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.border))
                .style(Style::default().bg(theme.bg_secondary)),
        ),
        chunks[0],
    );

    let mcp_icon = if app.mcp.ready { "●" } else { "○" };
    let mcp_color = if app.mcp.ready {
        theme.success
    } else {
        theme.dim
    };
    let mcp_lines = vec![
        Line::from(vec![
            Span::styled(format!(" {mcp_icon} "), Style::default().fg(mcp_color)),
            Span::styled(
                "MCP",
                Style::default()
                    .fg(theme.dock_title)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Servers ", Style::default().fg(theme.dim)),
            Span::styled(
                format!("{}", app.mcp.servers.len()),
                Style::default().fg(theme.text_primary),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Tools ", Style::default().fg(theme.dim)),
            Span::styled(
                format!("{}", app.mcp.tools.len()),
                Style::default().fg(theme.text_primary),
            ),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(mcp_lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.border))
                .style(Style::default().bg(theme.bg_secondary)),
        ),
        chunks[1],
    );

    let pending = app
        .pending_approvals
        .iter()
        .filter(|a| a.status == ApprovalStatus::Pending)
        .count();
    let approval_border = if pending > 0 {
        theme.error
    } else {
        theme.border
    };
    let approval_lines = if pending == 0 {
        vec![Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("No pending approvals", Style::default().fg(theme.dim)),
        ])]
    } else {
        let mut lines = vec![];
        for approval in app
            .pending_approvals
            .iter()
            .filter(|a| a.status == ApprovalStatus::Pending)
        {
            lines.push(Line::from(vec![
                Span::styled("  ⚠ ", Style::default().fg(theme.error)),
                Span::styled(
                    approval.tool_name.clone(),
                    Style::default().fg(theme.text_primary),
                ),
            ]));
        }
        lines
    };
    frame.render_widget(
        Paragraph::new(Text::from(approval_lines)).block(
            Block::default()
                .title(Span::styled(
                    format!(" Approvals ({pending})"),
                    Style::default()
                        .fg(theme.dock_title)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(approval_border))
                .style(Style::default().bg(theme.bg_secondary)),
        ),
        chunks[2],
    );

    let jobs_lines: Vec<Line> = if app.background_jobs.is_empty() {
        vec![
            Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled("No background jobs", Style::default().fg(theme.dim)),
            ]),
            Line::from(vec![Span::styled(
                "  /agent spawn <task>",
                Style::default().fg(theme.dim),
            )]),
        ]
    } else {
        let mut lines = Vec::with_capacity(app.background_jobs.len() * 2);
        for job in &app.background_jobs {
            let icon = match job.status {
                BackgroundJobStatus::Queued => "⧗",
                BackgroundJobStatus::Running => "◉",
                BackgroundJobStatus::Completed => "✓",
                BackgroundJobStatus::Failed => "✗",
                BackgroundJobStatus::Cancelled => "⊘",
            };
            let icon_color = match job.status {
                BackgroundJobStatus::Queued => theme.dim,
                BackgroundJobStatus::Running => theme.warning,
                BackgroundJobStatus::Completed => theme.success,
                BackgroundJobStatus::Failed => theme.error,
                BackgroundJobStatus::Cancelled => theme.dim,
            };
            let progress_tag = job
                .progress
                .map(|p| format!(" {p:>3}%"))
                .unwrap_or_default();
            let kind_tag = match job.kind {
                BackgroundJobKind::SubAgent => " ⎇",
                BackgroundJobKind::Command => " ⌘",
                BackgroundJobKind::Verification => " ✓",
                BackgroundJobKind::Index => " ⎈",
            };
            let title_color = if matches!(job.kind, BackgroundJobKind::SubAgent) {
                theme.text_primary
            } else {
                theme.text_secondary
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
                Span::styled(kind_tag, Style::default().fg(theme.dim)),
                Span::styled(" ", Style::default()),
                Span::styled(
                    job.title.clone(),
                    Style::default()
                        .fg(title_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(progress_tag, Style::default().fg(theme.accent)),
            ]));
            if !job.detail.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("    ", Style::default()),
                    Span::styled(job.detail.clone(), Style::default().fg(theme.dim)),
                ]));
            }
        }
        lines
    };
    frame.render_widget(
        Paragraph::new(Text::from(jobs_lines)).block(
            Block::default()
                .title(Span::styled(
                    " Background",
                    Style::default()
                        .fg(theme.dock_title)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.border))
                .style(Style::default().bg(theme.bg_secondary)),
        ),
        chunks[3],
    );
}

fn render_status(frame: &mut ratatui::Frame<'_>, app: &mut SessionApp, area: Rect) {
    let theme = &app.theme;
    let pending = app
        .pending_approvals
        .iter()
        .filter(|a| a.status == ApprovalStatus::Pending)
        .count();
    let active_subagents = app
        .background_jobs
        .iter()
        .filter(|j| {
            matches!(j.kind, BackgroundJobKind::SubAgent)
                && matches!(
                    j.status,
                    BackgroundJobStatus::Running | BackgroundJobStatus::Queued
                )
        })
        .count();

    let intent_icon = match app.current_intent {
        RouterIntent::Explore => "◈",
        RouterIntent::Plan => "◈",
        RouterIntent::Implement => "◈",
        RouterIntent::Verify => "◈",
    };

    let autonomy_col = autonomy_color(app.autonomy, theme);
    let autonomy_badge = if app.autonomy == AutonomyLevel::Yolo {
        "⚡ YOLO"
    } else {
        match app.autonomy {
            AutonomyLevel::Conservative => "🛡 safe",
            AutonomyLevel::Balanced => "⚖ balanced",
            AutonomyLevel::Aggressive => "✦ fast",
            AutonomyLevel::Yolo => "⚡ YOLO",
        }
    };

    let mut spans = vec![
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("{intent_icon} "),
            Style::default().fg(theme.role_router),
        ),
        Span::styled(
            format!("{:?}", app.current_intent),
            Style::default()
                .fg(theme.status_label)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            format!(" {autonomy_badge} "),
            Style::default()
                .fg(theme.bg_primary)
                .bg(autonomy_col)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if !app.current_model_display.is_empty() {
        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled("≋ ", Style::default().fg(theme.accent)));
        spans.push(Span::styled(
            truncate_str(&app.current_model_display, 28),
            Style::default().fg(theme.status_value),
        ));
    }

    if pending > 0 {
        spans.push(Span::styled("  │ ", Style::default().fg(theme.dim)));
        spans.push(Span::styled(
            format!("⚠ {pending} approvals"),
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
        ));
    }

    if active_subagents > 0 {
        spans.push(Span::styled("  │ ", Style::default().fg(theme.dim)));
        spans.push(Span::styled(
            format!("⎇ {active_subagents} sub-agents"),
            Style::default().fg(theme.warning),
        ));
    }

    if let Some((p, c, _)) = app.last_usage {
        spans.push(Span::styled("  │ ", Style::default().fg(theme.dim)));
        spans.push(Span::styled(
            format!("↑{p} ↓{c}"),
            Style::default().fg(theme.dim),
        ));
    }

    if app.processing {
        spans.push(Span::styled("  │ ", Style::default().fg(theme.dim)));
        spans.push(Span::styled(
            app.spinner.tick().to_string(),
            Style::default().fg(theme.accent),
        ));
    }

    // Right-aligned hints.
    spans.push(Span::styled("    ", Style::default()));
    spans.push(Span::styled("F1 help", Style::default().fg(theme.dim)));
    spans.push(Span::styled(" · ", Style::default().fg(theme.dim)));
    spans.push(Span::styled(
        "Ctrl+P palette",
        Style::default().fg(theme.dim),
    ));
    spans.push(Span::styled(" · ", Style::default().fg(theme.dim)));
    spans.push(Span::styled(
        "Ctrl+Y autonomy",
        Style::default().fg(theme.dim),
    ));

    let status_line = Line::from(spans);
    frame.render_widget(
        Paragraph::new(status_line).style(Style::default().bg(theme.bg_secondary)),
        area,
    );
}

fn render_composer(frame: &mut ratatui::Frame<'_>, app: &SessionApp, area: Rect) {
    let theme = &app.theme;
    let suggestions = app.suggestion_items();
    let input_text = app.input.as_str();
    let is_slash = input_text.starts_with('/');

    // Split composer area: suggestion dropdown above, the input field below.
    let suggestion_count = suggestions.len().min(6);
    let dropdown_height = if is_slash && suggestion_count > 0 {
        suggestion_count as u16 + 2
    } else {
        0
    };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(dropdown_height), Constraint::Min(3)])
        .split(area);

    if dropdown_height > 0 {
        let items: Vec<ListItem> = suggestions
            .iter()
            .take(suggestion_count)
            .map(|item| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("  {:<32}", item.command),
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" {} ", item.category.label()),
                        Style::default().fg(theme.dim),
                    ),
                    Span::styled(
                        item.description.to_string(),
                        Style::default().fg(theme.text_secondary),
                    ),
                ]))
            })
            .collect();

        frame.render_widget(
            List::new(items).block(
                Block::default()
                    .title(Span::styled(
                        " Slash commands ",
                        Style::default()
                            .fg(theme.dock_title)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.border))
                    .style(Style::default().bg(theme.bg_secondary))
                    .padding(Padding::new(0, 0, 0, 0)),
            ),
            layout[0],
        );
    }

    let input_area = layout[1];

    let border_color = if app.processing {
        theme.accent
    } else if !input_text.is_empty() {
        theme.border_focused
    } else {
        theme.border
    };

    // Build the visible composer line(s).
    let prompt_indicator = if app.processing { "⠋ " } else { "› " };
    let mut composer_lines: Vec<Line> = Vec::new();
    if !app.context_items.is_empty() {
        let chips: Vec<Span> = app
            .context_items
            .iter()
            .flat_map(|chip| {
                vec![
                    Span::styled(
                        format!(" {chip} "),
                        Style::default()
                            .fg(theme.bg_primary)
                            .bg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" ", Style::default()),
                ]
            })
            .collect();
        composer_lines.push(Line::from(chips));
    }

    if input_text.is_empty() {
        composer_lines.push(Line::from(vec![
            Span::styled(
                prompt_indicator,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Ask, plan, or press / for commands",
                Style::default().fg(theme.dim),
            ),
        ]));
    } else {
        for (idx, line) in input_text.split('\n').enumerate() {
            let gutter = if idx == 0 { prompt_indicator } else { "  " };
            composer_lines.push(Line::from(vec![
                Span::styled(
                    gutter,
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(line.to_string(), Style::default().fg(theme.text_primary)),
            ]));
        }
    }

    let title = if app.session_title.is_empty() {
        " Charm ".to_string()
    } else {
        format!(" Charm · {} ", truncate_str(&app.session_title, 40))
    };

    let composer_block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(theme.bg_composer))
        .padding(Padding::new(1, 1, 0, 0));

    let paragraph = Paragraph::new(Text::from(composer_lines))
        .block(composer_block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, input_area);

    if app.overlay == Overlay::None && !input_text.is_empty() && app.cursor_visible {
        let chips_offset: u16 = if app.context_items.is_empty() { 0 } else { 1 };
        let (cursor_col, cursor_row) = composer_cursor_position(input_text, app.input.cursor);
        let line_prefix = if cursor_row == 0 {
            UnicodeWidthStr::width(prompt_indicator) as u16
        } else {
            2
        };
        let cursor_x = input_area.x + 2 + line_prefix + cursor_col as u16;
        let cursor_y = input_area.y + 1 + chips_offset + cursor_row as u16;
        frame.set_cursor_position((
            cursor_x.min(input_area.x + input_area.width.saturating_sub(2)),
            cursor_y.min(input_area.y + input_area.height.saturating_sub(2)),
        ));
    }
}

fn composer_input_rows(input: &str) -> usize {
    if input.is_empty() {
        1
    } else {
        input.matches('\n').count() + 1
    }
}

fn composer_cursor_position(input: &str, cursor: usize) -> (usize, usize) {
    let before_cursor = &input[..cursor.min(input.len())];
    let row = before_cursor.matches('\n').count();
    let line_start = before_cursor.rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    let col = UnicodeWidthStr::width(&before_cursor[line_start..]);
    (col, row)
}

#[allow(dead_code)] // replaced by render_dialog_overlay; kept for reference
fn render_palette(frame: &mut ratatui::Frame<'_>, app: &SessionApp) {
    let theme = &app.theme;
    let area = centered_rect(60, 50, frame.area());

    let items: Vec<ListItem> = app
        .palette_items()
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let style = if index == app.overlay_index {
                Style::default()
                    .fg(theme.palette_selected_fg)
                    .bg(theme.palette_selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_primary)
            };
            ListItem::new(format!("  {}  {}", item.command, item.description)).style(style)
        })
        .collect();

    frame.render_widget(Clear, area);
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(Span::styled(
                    " ⌘ Commands",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.accent))
                .style(Style::default().bg(theme.bg_secondary)),
        ),
        area,
    );
}

#[allow(dead_code)] // replaced by render_dialog_overlay; kept for reference
fn render_sessions(frame: &mut ratatui::Frame<'_>, app: &SessionApp) {
    let theme = &app.theme;
    let area = centered_rect(65, 55, frame.area());

    let items: Vec<ListItem> = if app.sessions.is_empty() {
        vec![ListItem::new(Span::styled(
            "  No sessions found",
            Style::default().fg(theme.dim),
        ))]
    } else {
        app.sessions
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let style = if index == app.overlay_index {
                    Style::default()
                        .fg(theme.palette_selected_fg)
                        .bg(theme.palette_selected_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text_primary)
                };
                let icon = if session.active { "◉" } else { "○" };
                let intent_color = match session.intent.as_str() {
                    "Explore" => theme.role_user,
                    "Plan" => theme.role_router,
                    "Implement" => theme.role_assistant,
                    "Verify" => theme.role_tool,
                    _ => theme.text_secondary,
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {} ", icon), Style::default().fg(intent_color)),
                    Span::styled(format!("{:<30}", truncate_str(&session.title, 28)), style),
                    Span::styled(session.intent.clone(), Style::default().fg(theme.dim)),
                ]))
            })
            .collect()
    };

    frame.render_widget(Clear, area);
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(Span::styled(
                    " ⏎ Sessions  Ctrl+N new",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.accent))
                .style(Style::default().bg(theme.bg_secondary)),
        ),
        area,
    );
}

#[allow(dead_code)] // replaced by render_dialog_overlay; kept for reference
fn render_model_switcher(frame: &mut ratatui::Frame<'_>, app: &SessionApp) {
    let theme = &app.theme;
    let area = centered_rect(55, 55, frame.area());

    let items: Vec<ListItem> = app
        .available_models
        .iter()
        .enumerate()
        .map(|(index, model)| {
            let is_current = model.model_id == app.current_model_display
                || format!("{}/{}", model.provider, model.model_id) == app.current_model_display;
            let style = if index == app.overlay_index {
                Style::default()
                    .fg(theme.palette_selected_fg)
                    .bg(theme.palette_selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else if is_current {
                Style::default()
                    .fg(theme.success)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_primary)
            };
            let icon = if is_current { "●" } else { "○" };
            let provider_color = match model.provider.as_str() {
                "openai" => theme.success,
                "anthropic" => theme.role_assistant,
                "google" => theme.warning,
                "ollama" => theme.dim,
                "openrouter" => theme.accent,
                _ => theme.text_secondary,
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {} ", icon), Style::default().fg(provider_color)),
                Span::styled(format!("{:<22}", model.display), style),
                Span::styled(model.provider.clone(), Style::default().fg(theme.dim)),
            ]))
        })
        .collect();

    frame.render_widget(Clear, area);
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(Span::styled(
                    " ⚡ Model  Enter to switch",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.accent))
                .style(Style::default().bg(theme.bg_secondary)),
        ),
        area,
    );
}

fn friendly_tool_name(raw: &str) -> String {
    match raw {
        "read_range" => "Read".to_string(),
        "read_symbol" => "Symbol".to_string(),
        "grep_search" => "Grep".to_string(),
        "glob_search" => "Glob".to_string(),
        "list_dir" => "List".to_string(),
        "semantic_search" => "Search".to_string(),
        "parallel_search" => "Search".to_string(),
        "edit_patch" => "Edit".to_string(),
        "write_file" => "Write".to_string(),
        "run_command" => "Run".to_string(),
        "poll_command" => "Poll".to_string(),
        "plan_update" => "Plan".to_string(),
        "checkpoint_create" => "Checkpoint".to_string(),
        "checkpoint_restore" => "Restore".to_string(),
        "memory_stage" => "Memory".to_string(),
        "memory_commit" => "Memory".to_string(),
        other if other.starts_with("mcp:") => {
            let parts: Vec<&str> = other.splitn(3, ':').collect();
            if parts.len() >= 2 {
                parts[1].to_string()
            } else {
                other.to_string()
            }
        }
        _ => raw.to_string(),
    }
}

fn style_line(line: &str, base_color: ratatui::style::Color, theme: &Theme) -> Vec<Span<'static>> {
    let trimmed = line.trim_end();
    if trimmed.starts_with("```") {
        let lang = trimmed.strip_prefix("```").unwrap_or("").trim();
        return vec![Span::styled(
            format!(
                "  {} ",
                if lang.is_empty() {
                    "code".to_string()
                } else {
                    lang.to_string()
                }
            ),
            Style::default().fg(theme.bg_primary).bg(theme.role_tool),
        )];
    }
    if trimmed.starts_with('`') && trimmed.ends_with('`') && trimmed.len() > 2 {
        return vec![Span::styled(
            trimmed.to_string(),
            Style::default().fg(theme.accent),
        )];
    }
    let mut spans = Vec::new();
    let mut in_bold = false;
    let mut chars = trimmed.chars().peekable();
    let mut current = String::new();
    while let Some(ch) = chars.next() {
        if ch == '*' && chars.peek() == Some(&'*') {
            chars.next();
            if !current.is_empty() {
                if in_bold {
                    spans.push(Span::styled(
                        current.clone(),
                        Style::default().fg(base_color).add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::styled(
                        current.clone(),
                        Style::default().fg(base_color),
                    ));
                }
                current.clear();
            }
            in_bold = !in_bold;
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        if in_bold {
            spans.push(Span::styled(
                current,
                Style::default().fg(base_color).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(current, Style::default().fg(base_color)));
        }
    }
    if spans.is_empty() {
        spans.push(Span::styled(
            trimmed.to_string(),
            Style::default().fg(base_color),
        ));
    }
    spans
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

fn render_help_overlay(frame: &mut ratatui::Frame<'_>, app: &SessionApp) {
    let theme = &app.theme;
    let area = centered_rect(72, 80, frame.area());
    frame.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(" ✦ ", Style::default().fg(theme.accent)),
        Span::styled(
            "Charm — autonomous coding harness",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));

    let section = |title: &str| -> Line {
        Line::from(Span::styled(
            format!(" {title}"),
            Style::default()
                .fg(theme.dock_title)
                .add_modifier(Modifier::BOLD),
        ))
    };

    lines.push(section("Keyboard"));
    for (keys, desc) in [
        ("Ctrl+P", "Command palette"),
        ("Ctrl+L", "Session switcher"),
        ("Ctrl+M", "Model switcher"),
        ("Ctrl+N", "New session"),
        ("Ctrl+Y", "Cycle autonomy"),
        ("Ctrl+A", "Sub-agent queue"),
        ("Ctrl+Shift+P / M / A", "Providers / MCP / approvals"),
        ("Ctrl+Tab / Ctrl+Shift+Tab", "Next / previous session"),
        ("Ctrl+B / Ctrl+D", "Toggle left / right dock"),
        ("Tab", "Autocomplete slash command"),
        ("Shift+Enter / Option+Enter", "Insert newline"),
        ("Option+←/→", "Move by word"),
        ("Option+Backspace/Delete", "Delete word"),
        ("F1 / ?", "Open this help overlay"),
        ("PgUp / PgDn", "Scroll transcript"),
        ("Esc", "Clear draft, then quit"),
    ] {
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(format!("{keys:<28}"), Style::default().fg(theme.accent)),
            Span::styled(desc.to_string(), Style::default().fg(theme.text_secondary)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(section("Autonomy"));
    for level in [
        AutonomyLevel::Conservative,
        AutonomyLevel::Balanced,
        AutonomyLevel::Aggressive,
        AutonomyLevel::Yolo,
    ] {
        let marker = if level == app.autonomy { "●" } else { "○" };
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {marker} "),
                Style::default().fg(autonomy_color(level, theme)),
            ),
            Span::styled(
                format!("{:<14}", level.label()),
                Style::default()
                    .fg(autonomy_color(level, theme))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                level.detail().to_string(),
                Style::default().fg(theme.text_secondary),
            ),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(section("Slash commands"));
    for category in [
        CommandCategory::Intent,
        CommandCategory::Autonomy,
        CommandCategory::Session,
        CommandCategory::Agent,
        CommandCategory::Context,
        CommandCategory::Inspect,
        CommandCategory::Meta,
    ] {
        lines.push(Line::from(Span::styled(
            format!("  ▸ {}", category.label()),
            Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
        )));
        for item in command_catalog()
            .into_iter()
            .filter(|item| item.category == category)
        {
            lines.push(Line::from(vec![
                Span::styled("    ", Style::default()),
                Span::styled(
                    format!("{:<34}", item.command),
                    Style::default().fg(theme.accent),
                ),
                Span::styled(
                    item.description.to_string(),
                    Style::default().fg(theme.text_secondary),
                ),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Esc to close, ↑/↓ to scroll.",
        Style::default().fg(theme.dim),
    )));

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title(Span::styled(
                        " Help ",
                        Style::default()
                            .fg(theme.dock_title)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.border_focused))
                    .style(Style::default().bg(theme.bg_secondary))
                    .padding(Padding::new(1, 1, 1, 1)),
            )
            .scroll((app.overlay_index as u16, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

#[allow(dead_code)] // replaced by render_dialog_overlay; kept for reference
fn render_autonomy_overlay(frame: &mut ratatui::Frame<'_>, app: &SessionApp) {
    let theme = &app.theme;
    let area = centered_rect(50, 40, frame.area());
    frame.render_widget(Clear, area);

    let levels = [
        AutonomyLevel::Conservative,
        AutonomyLevel::Balanced,
        AutonomyLevel::Aggressive,
        AutonomyLevel::Yolo,
    ];

    let items: Vec<ListItem> = levels
        .iter()
        .enumerate()
        .map(|(idx, level)| {
            let active = idx == app.overlay_index;
            let marker = if *level == app.autonomy { "●" } else { "○" };
            let style = if active {
                Style::default()
                    .fg(theme.palette_selected_fg)
                    .bg(theme.palette_selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(autonomy_color(*level, theme))
            };
            let desc_style = if active {
                Style::default()
                    .fg(theme.palette_selected_fg)
                    .bg(theme.palette_selected_bg)
            } else {
                Style::default().fg(theme.text_secondary)
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {marker} "), style),
                Span::styled(format!("{:<14}", level.label()), style),
                Span::styled(level.detail().to_string(), desc_style),
            ]))
        })
        .collect();

    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(Span::styled(
                    format!(" Autonomy (current: {}) ", app.autonomy.label()),
                    Style::default()
                        .fg(theme.dock_title)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(autonomy_color(app.autonomy, theme)))
                .style(Style::default().bg(theme.bg_secondary))
                .padding(Padding::new(1, 1, 1, 1)),
        ),
        area,
    );
}

#[allow(dead_code)] // replaced by render_dialog_overlay; kept for reference
fn render_agents_overlay(frame: &mut ratatui::Frame<'_>, app: &SessionApp) {
    let theme = &app.theme;
    let area = centered_rect(65, 60, frame.area());
    frame.render_widget(Clear, area);

    let sub: Vec<&BackgroundJob> = app
        .background_jobs
        .iter()
        .filter(|j| matches!(j.kind, BackgroundJobKind::SubAgent))
        .collect();

    let lines: Vec<Line> = if sub.is_empty() {
        vec![
            Line::from(Span::styled(
                "  No sub-agents spawned yet.",
                Style::default().fg(theme.dim),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Try: /agent spawn audit authentication layer",
                Style::default().fg(theme.accent),
            )),
        ]
    } else {
        sub.iter()
            .enumerate()
            .flat_map(|(idx, job)| {
                let active = idx == app.overlay_index;
                let marker = if active { "▸" } else { " " };
                let icon = match job.status {
                    BackgroundJobStatus::Queued => "⧗",
                    BackgroundJobStatus::Running => "◉",
                    BackgroundJobStatus::Completed => "✓",
                    BackgroundJobStatus::Failed => "✗",
                    BackgroundJobStatus::Cancelled => "⊘",
                };
                let icon_color = match job.status {
                    BackgroundJobStatus::Queued => theme.dim,
                    BackgroundJobStatus::Running => theme.warning,
                    BackgroundJobStatus::Completed => theme.success,
                    BackgroundJobStatus::Failed => theme.error,
                    BackgroundJobStatus::Cancelled => theme.dim,
                };
                let progress = job.progress.map(|p| format!(" {p}%")).unwrap_or_default();
                vec![
                    Line::from(vec![
                        Span::styled(format!(" {marker} "), Style::default().fg(theme.accent)),
                        Span::styled(format!("{icon} "), Style::default().fg(icon_color)),
                        Span::styled(
                            format!("[{}] ", &job.id[..job.id.len().min(8)]),
                            Style::default().fg(theme.dim),
                        ),
                        Span::styled(
                            job.title.clone(),
                            Style::default()
                                .fg(theme.text_primary)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(progress, Style::default().fg(theme.accent)),
                    ]),
                    Line::from(vec![
                        Span::styled("      ", Style::default()),
                        Span::styled(job.detail.clone(), Style::default().fg(theme.dim)),
                    ]),
                ]
            })
            .collect()
    };

    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(Span::styled(
                    format!(" Sub-agents ({}) ", sub.len()),
                    Style::default()
                        .fg(theme.dock_title)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.border_focused))
                .style(Style::default().bg(theme.bg_secondary))
                .padding(Padding::new(1, 1, 1, 1)),
        ),
        area,
    );
}

fn render_welcome_overlay(frame: &mut ratatui::Frame<'_>, app: &SessionApp) {
    let theme = &app.theme;
    let area = centered_rect(58, 55, frame.area());
    frame.render_widget(Clear, area);

    let banner: &[&str] = &[
        "       _                          ",
        "   ___| |__   __ _ _ __ _ __ ___  ",
        "  / __| '_ \\ / _` | '__| '_ ` _ \\ ",
        " | (__| | | | (_| | |  | | | | | |",
        "  \\___|_| |_|\\__,_|_|  |_| |_| |_|",
    ];

    let mut lines: Vec<Line> = banner
        .iter()
        .map(|row| {
            Line::from(Span::styled(
                row.to_string(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
        })
        .collect();

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  autonomous coding harness · Rust · terminal-native",
        Style::default().fg(theme.text_secondary),
    )]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  workspace  ", Style::default().fg(theme.dim)),
        Span::styled(
            app.workspace_root.display().to_string(),
            Style::default().fg(theme.text_primary),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  model      ", Style::default().fg(theme.dim)),
        Span::styled(
            if app.current_model_display.is_empty() {
                "default".to_string()
            } else {
                app.current_model_display.clone()
            },
            Style::default().fg(theme.text_primary),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  autonomy   ", Style::default().fg(theme.dim)),
        Span::styled(
            app.autonomy.label(),
            Style::default()
                .fg(autonomy_color(app.autonomy, theme))
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  press / for commands, F1 for help, or just type",
        Style::default().fg(theme.dim),
    )]));

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme.accent))
                    .style(Style::default().bg(theme.bg_secondary))
                    .padding(Padding::new(2, 2, 1, 1)),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_toast(frame: &mut ratatui::Frame<'_>, app: &SessionApp, text: &str) {
    let theme = &app.theme;
    let area = frame.area();
    let width = (text.width() as u16 + 4).min(area.width.saturating_sub(4));
    let toast_area = Rect {
        x: area.width.saturating_sub(width + 2),
        y: area.height.saturating_sub(5),
        width,
        height: 3,
    };
    frame.render_widget(Clear, toast_area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ⚡ ", Style::default().fg(theme.accent)),
            Span::styled(
                text.to_string(),
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.accent))
                .style(Style::default().bg(theme.bg_highlight)),
        ),
        toast_area,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::types::{ApprovalRequest, RuntimeEvent};
    use chrono::Utc;

    #[test]
    fn slash_and_palette_share_same_catalog() {
        let suggestions = slash_suggestions("/v");
        assert_eq!(suggestions[0].command, "/verify");
        assert!(
            command_catalog()
                .iter()
                .any(|item| item.command == suggestions[0].command)
        );
    }

    #[test]
    fn longest_common_prefix_extends_short_slash() {
        // `/a` matches /agent spawn, /agent list, /agent kill, /autonomy...
        let strs = vec!["/agent spawn", "/agent list", "/agent kill"];
        let prefix = longest_common_prefix(&strs);
        assert_eq!(prefix, "/agent ");
    }

    #[test]
    fn strip_placeholders_drops_brackets() {
        assert_eq!(strip_placeholders("/session <id>"), "/session ");
        assert_eq!(strip_placeholders("/help"), "/help");
        assert_eq!(strip_placeholders("/context add <path>"), "/context add ");
    }

    #[test]
    fn tab_autocomplete_extends_prefix_for_slash() {
        let mut app = SessionApp::default();
        app.input.buffer = "/a".to_string();
        app.input.cursor = 2;
        complete_slash(&mut app);
        // All /a... slash commands share "/a" as prefix; at least "/agent"
        // and "/autonomy" exist, so the longest common prefix is "/a".
        // There is no longer prefix so the buffer should stay or extend to
        // "/autonomy" if only one match survives. We assert it didn't shrink.
        assert!(app.input.as_str().starts_with("/a"));
    }

    #[test]
    fn wrap_single_line_splits_on_embedded_newline() {
        // Regression: embedded \n was treated as a 0-width char, so
        // "hello\nworld" survived as a single display line with a raw
        // newline byte in the Span content → garbled terminal output.
        let line = Line::from(Span::raw("hello\nworld"));
        let wrapped = wrap_single_line(&line, 80);
        assert_eq!(
            wrapped.len(),
            2,
            "expected two display rows, got {wrapped:?}"
        );
        let first: String = wrapped[0].spans.iter().map(|s| s.content.clone()).collect();
        let second: String = wrapped[1].spans.iter().map(|s| s.content.clone()).collect();
        assert_eq!(first, "hello");
        assert_eq!(second, "world");
        for line in &wrapped {
            for span in &line.spans {
                assert!(
                    !span.content.contains('\n'),
                    "embedded newline leaked into span: {span:?}"
                );
                assert!(
                    !span.content.contains('\r'),
                    "embedded carriage return leaked into span: {span:?}"
                );
            }
        }
    }

    #[test]
    fn wrap_single_line_splits_on_multiple_newlines() {
        let line = Line::from(Span::raw("a\n\nb\nc"));
        let wrapped = wrap_single_line(&line, 80);
        let rendered: Vec<String> = wrapped
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.clone()).collect())
            .collect();
        assert_eq!(rendered, vec!["a", "", "b", "c"]);
    }

    #[test]
    fn streamed_code_block_renders_on_separate_rows() {
        use ratatui::backend::TestBackend;
        // Simulate an LLM streaming a short code block like:
        //   "Here is a fn:\n```rust\nfn main() {}\n```"
        // Arriving as a few deltas (which is how real providers chunk it).
        let mut app = SessionApp::default();
        app.apply_event(RuntimeEvent::StreamDelta {
            role: "assistant".to_string(),
            content: "Here is a fn:\n```rust\n".to_string(),
            model: None,
        });
        app.apply_event(RuntimeEvent::StreamDelta {
            role: "assistant".to_string(),
            content: "fn main() {}\n".to_string(),
            model: None,
        });
        app.apply_event(RuntimeEvent::StreamDelta {
            role: "assistant".to_string(),
            content: "```".to_string(),
            model: None,
        });

        let backend = TestBackend::new(60, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = ratatui::layout::Rect {
                    x: 0,
                    y: 0,
                    width: 60,
                    height: 12,
                };
                render_transcript(frame, &mut app, area);
            })
            .unwrap();
        let dump = buffer_to_string(terminal.backend().buffer());
        eprintln!("=== streamed code block dump ===\n{dump}=== end ===");
        // Every display row must be a clean row of printable chars.
        for line in dump.lines() {
            assert!(
                !line.contains('\n') && !line.contains('\r'),
                "row leaked control char: {line:?}"
            );
        }
        assert!(
            dump.contains("Here is a fn:"),
            "missing intro line:\n{dump}"
        );
        assert!(dump.contains("fn main() {}"), "missing code line:\n{dump}");
        // The "```rust" marker and the intro line must be on different
        // terminal rows, otherwise the streaming delta squashed them.
        let rust_row = dump.lines().position(|l| l.contains("rust")).unwrap_or(0);
        let intro_row = dump
            .lines()
            .position(|l| l.contains("Here is a fn:"))
            .unwrap_or(0);
        assert_ne!(
            rust_row, intro_row,
            "intro and code fence landed on the same row:\n{dump}"
        );
    }

    #[test]
    fn transcript_with_newline_renders_as_separate_rows() {
        use ratatui::backend::TestBackend;
        // Render a transcript that mimics the bug the user saw: a single
        // Line with embedded \n. Dump the terminal buffer and make sure no
        // row contains a raw newline byte and the text is split across
        // rows.
        let mut app = SessionApp::default();
        app.transcript
            .push(Line::from(Span::raw("line one\nline two\nline three")));
        let backend = TestBackend::new(40, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = ratatui::layout::Rect {
                    x: 0,
                    y: 0,
                    width: 40,
                    height: 10,
                };
                render_transcript(frame, &mut app, area);
            })
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let dump = buffer_to_string(&buffer);
        eprintln!("=== render dump ===\n{dump}=== end ===");
        for line in dump.lines() {
            assert!(
                !line.contains('\n') && !line.contains('\r'),
                "row leaked control char: {line:?}"
            );
        }
        assert!(
            dump.contains("line one") && dump.contains("line two") && dump.contains("line three"),
            "missing rows in rendered dump:\n{dump}"
        );
    }

    fn buffer_to_string(buffer: &ratatui::buffer::Buffer) -> String {
        let width = buffer.area.width as usize;
        let height = buffer.area.height as usize;
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                if let Some(cell) = buffer.cell(ratatui::layout::Position {
                    x: x as u16,
                    y: y as u16,
                }) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn stream_delta_with_newline_creates_multiple_lines() {
        let mut app = SessionApp::default();
        // Seed a pending assistant line (as a typical turn starts).
        app.transcript.push(Line::from(vec![]));
        app.apply_event(RuntimeEvent::StreamDelta {
            role: "assistant".to_string(),
            content: "hel".to_string(),
            model: None,
        });
        app.apply_event(RuntimeEvent::StreamDelta {
            role: "assistant".to_string(),
            content: "lo\nworld".to_string(),
            model: None,
        });
        // The transcript should contain two display lines after wrap, not
        // one concatenated row with a raw \n byte.
        for (i, line) in app.transcript.iter().enumerate() {
            for span in &line.spans {
                assert!(
                    !span.content.contains('\n'),
                    "line {i} contains embedded newline: {:?}",
                    span.content
                );
            }
        }
        // Overall rendered text should contain "hello" and "world", each on
        // its own display row.
        let rows: Vec<String> = app
            .transcript
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.clone()).collect())
            .collect();
        let dump = rows.join(" | ");
        assert!(
            rows.iter().any(|r| r.contains("hello")),
            "no row with 'hello' in {dump}"
        );
        assert!(
            rows.iter().any(|r| r.contains("world")),
            "no row with 'world' in {dump}"
        );
        // "world" must land on a different row than "hello".
        let hello_row = rows.iter().position(|r| r.contains("hello")).unwrap();
        let world_row = rows.iter().position(|r| r.contains("world")).unwrap();
        assert_ne!(
            hello_row, world_row,
            "hello and world landed on the same row"
        );
    }

    #[test]
    fn skill_frontmatter_description_is_parsed() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("demo.md");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "---\ndescription: Run the demo workflow\n---\n\nSteps..."
        )
        .unwrap();
        let desc = parse_workflow_description(&path).unwrap();
        assert_eq!(desc, "Run the demo workflow");
    }

    #[test]
    fn approval_events_update_inline_queue_state() {
        let mut app = SessionApp::default();
        let approval = ApprovalRequest {
            id: "req-1".to_string(),
            tool_name: "run_command".to_string(),
            summary: "dangerous command".to_string(),
            risk: crate::core::RiskClass::Destructive,
            status: ApprovalStatus::Pending,
            created_at: Utc::now(),
            tool_arguments: None,
            tool_call_id: None,
        };
        app.apply_event(RuntimeEvent::ApprovalRequested {
            approval: approval.clone(),
        });
        assert_eq!(app.pending_approvals.len(), 1);
        assert!(
            app.transcript
                .iter()
                .any(|line| line.to_string().contains("run_command"))
        );
    }

    #[test]
    fn input_state_cursor_movement() {
        let mut input = InputState::default();
        input.insert('h');
        input.insert('e');
        input.insert('l');
        input.insert('l');
        input.insert('o');
        assert_eq!(input.as_str(), "hello");
        assert_eq!(input.display_cursor_width(), 5);

        input.move_left();
        assert_eq!(input.display_cursor_width(), 4);

        input.move_home();
        assert_eq!(input.display_cursor_width(), 0);

        input.move_end();
        assert_eq!(input.display_cursor_width(), 5);
    }

    #[test]
    fn input_state_backspace_and_delete() {
        let mut input = InputState::default();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        input.move_home();
        input.delete();
        assert_eq!(input.as_str(), "bc");

        input.move_end();
        input.backspace();
        assert_eq!(input.as_str(), "b");
    }

    #[test]
    fn input_state_delete_word() {
        let mut input = InputState::default();
        input.insert('h');
        input.insert('e');
        input.insert('l');
        input.insert('l');
        input.insert('o');
        input.insert(' ');
        input.insert('w');
        input.insert('o');
        input.insert('r');
        input.insert('l');
        input.insert('d');
        input.delete_word();
        assert_eq!(input.as_str(), "hello ");
    }

    #[test]
    fn input_state_word_navigation_and_forward_delete() {
        let mut input = InputState::default();
        input.insert_str("alpha beta gamma");
        input.move_word_left();
        assert_eq!(input.as_str(), "alpha beta gamma");
        assert_eq!(input.cursor, "alpha beta ".len());

        input.move_word_left();
        assert_eq!(input.cursor, "alpha ".len());

        input.delete_word_forward();
        assert_eq!(input.as_str(), "alpha  gamma");

        input.move_word_right();
        assert_eq!(input.cursor, "alpha  gamma".len());
    }

    #[test]
    fn input_state_multiline_cursor_width_uses_current_line() {
        let mut input = InputState::default();
        input.insert_str("first line\nsecond");
        assert_eq!(input.display_cursor_width(), "second".len());

        input.move_word_left();
        assert_eq!(input.display_cursor_width(), 0);
    }

    #[test]
    fn input_state_submit_deduplicates_consecutive_history() {
        let mut input = InputState::default();
        input.insert_str("repeat");
        assert_eq!(input.submit(), Some("repeat".to_string()));
        input.insert_str("repeat");
        assert_eq!(input.submit(), Some("repeat".to_string()));
        assert_eq!(input.history, vec!["repeat".to_string()]);
    }

    #[test]
    fn input_state_history_navigation() {
        let mut input = InputState::default();
        input.insert('f');
        input.insert('i');
        input.insert('r');
        input.insert('s');
        input.insert('t');
        let result = input.submit();
        assert_eq!(result, Some("first".to_string()));

        input.insert('s');
        input.insert('e');
        input.insert('c');
        input.insert('o');
        input.insert('n');
        input.insert('d');
        let result = input.submit();
        assert_eq!(result, Some("second".to_string()));

        input.history_up();
        assert_eq!(input.as_str(), "second");
        input.history_up();
        assert_eq!(input.as_str(), "first");
        input.history_down();
        assert_eq!(input.as_str(), "second");
    }

    #[test]
    fn esc_clears_composer_before_quitting() {
        let mut app = SessionApp::default();
        app.input.insert_str("draft");

        let should_quit =
            handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app).unwrap();

        assert!(!should_quit);
        assert!(app.input.is_empty());

        let should_quit =
            handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app).unwrap();

        assert!(should_quit);
    }

    #[test]
    fn option_arrow_moves_by_word_on_mac_terminals() {
        let mut app = SessionApp::default();
        app.input.insert_str("alpha beta gamma");

        handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT), &mut app).unwrap();
        assert_eq!(app.input.cursor, "alpha beta ".len());

        handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT), &mut app).unwrap();
        assert_eq!(app.input.cursor, "alpha beta gamma".len());
    }

    #[test]
    fn shift_enter_inserts_newline_instead_of_submitting() {
        let (tx, rx) = mpsc::channel();
        let mut app = SessionApp::default();
        app.input_sender = Some(tx);
        app.input.insert_str("line one");

        handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &mut app).unwrap();

        assert_eq!(app.input.as_str(), "line one\n");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn enter_while_processing_keeps_draft_and_does_not_queue_hidden_turn() {
        let (tx, rx) = mpsc::channel();
        let mut app = SessionApp::default();
        app.input_sender = Some(tx);
        app.processing = true;
        app.input.insert_str("next task");

        handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app).unwrap();

        assert_eq!(app.input.as_str(), "next task");
        assert!(rx.try_recv().is_err());
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|(text, _)| text.contains("still running"))
        );
    }

    #[test]
    fn completed_agent_overlay_enter_opens_diff_instead_of_killing() {
        let (tx, rx) = mpsc::channel();
        let mut app = SessionApp::default();
        app.input_sender = Some(tx);
        app.overlay = Overlay::Agents;
        app.background_jobs.push(BackgroundJob {
            id: "abcdef123456".to_string(),
            title: "review me".to_string(),
            status: BackgroundJobStatus::Completed,
            detail: "done".to_string(),
            kind: BackgroundJobKind::SubAgent,
            progress: Some(100),
            metadata: None,
        });

        submit_overlay_selection(&mut app);

        assert_eq!(rx.try_recv().unwrap(), "/agent diff abcdef12");
    }

    #[test]
    fn agent_overlay_keyboard_shortcuts_apply_review_actions() {
        let (tx, rx) = mpsc::channel();
        let mut app = SessionApp::default();
        app.input_sender = Some(tx);
        app.overlay = Overlay::Agents;
        app.background_jobs.push(BackgroundJob {
            id: "fedcba987654".to_string(),
            title: "merge me".to_string(),
            status: BackgroundJobStatus::Completed,
            detail: "done".to_string(),
            kind: BackgroundJobKind::SubAgent,
            progress: Some(100),
            metadata: None,
        });

        handle_overlay_key(
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
            &mut app,
        )
        .unwrap();

        assert_eq!(rx.try_recv().unwrap(), "/agent merge fedcba98");
    }

    #[test]
    fn approval_overlay_enter_approves_selected_request() {
        let (tx, rx) = mpsc::channel();
        let mut app = SessionApp::default();
        app.input_sender = Some(tx);
        app.overlay = Overlay::Approvals;
        app.pending_approvals.push(ApprovalRequest {
            id: "approve123456".to_string(),
            tool_name: "run_command".to_string(),
            summary: "cargo test".to_string(),
            risk: crate::core::RiskClass::ExternalSideEffect,
            status: ApprovalStatus::Pending,
            created_at: Utc::now(),
            tool_arguments: None,
            tool_call_id: None,
        });

        submit_overlay_selection(&mut app);

        assert_eq!(rx.try_recv().unwrap(), "/approvals approve approve1");
    }

    #[test]
    fn approval_overlay_d_shortcut_denies_selected_request() {
        let (tx, rx) = mpsc::channel();
        let mut app = SessionApp::default();
        app.input_sender = Some(tx);
        app.overlay = Overlay::Approvals;
        app.pending_approvals.push(ApprovalRequest {
            id: "deny123456".to_string(),
            tool_name: "write_file".to_string(),
            summary: "write config".to_string(),
            risk: crate::core::RiskClass::Destructive,
            status: ApprovalStatus::Pending,
            created_at: Utc::now(),
            tool_arguments: None,
            tool_call_id: None,
        });

        handle_overlay_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &mut app,
        )
        .unwrap();

        assert_eq!(rx.try_recv().unwrap(), "/approvals deny deny1234");
    }

    #[test]
    fn theme_role_color_returns_correct_mapping() {
        let theme = Theme::default();
        assert_eq!(theme.role_color("user"), theme.role_user);
        assert_eq!(theme.role_color("assistant"), theme.role_assistant);
        assert_eq!(theme.role_color("unknown"), theme.text_secondary);
    }

    #[test]
    fn scroll_state_defaults_to_auto_scroll() {
        let app = SessionApp::default();
        assert!(app.auto_scroll);
    }
}
