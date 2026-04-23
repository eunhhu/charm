use crate::runtime::session_runtime::SessionRuntime;
use crate::runtime::types::{
    ApprovalRequest, ApprovalStatus, AutonomyLevel, BackgroundJob, LspSnapshot, McpSnapshot,
    RouterIntent, RuntimeEvent, SessionLifecycle, WorkspacePreflight,
};
use crate::tui::event::{AppEvent, EventBridge};
use crate::tui::theme::Theme;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap};
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

    pub fn delete_word(&mut self) {
        let start = self.buffer[..self.cursor]
            .char_indices()
            .rev()
            .skip_while(|(_, ch)| ch.is_whitespace())
            .find(|(_, ch)| ch.is_whitespace())
            .map(|(i, _)| i + 1)
            .unwrap_or(0);
        self.buffer.drain(start..self.cursor);
        self.cursor = start;
    }

    pub fn submit(&mut self) -> Option<String> {
        if self.buffer.trim().is_empty() {
            return None;
        }
        let input = std::mem::take(&mut self.buffer);
        self.cursor = 0;
        self.history.push(input.clone());
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
        UnicodeWidthStr::width(&self.buffer[..self.cursor])
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandItem {
    pub command: &'static str,
    pub description: &'static str,
}

pub fn command_catalog() -> Vec<CommandItem> {
    vec![
        CommandItem {
            command: "/plan",
            description: "Force planning intent for the current turn",
        },
        CommandItem {
            command: "/explore",
            description: "Force exploration intent for the current turn",
        },
        CommandItem {
            command: "/build",
            description: "Force implementation intent for the current turn",
        },
        CommandItem {
            command: "/verify",
            description: "Force verification intent for the current turn",
        },
        CommandItem {
            command: "/approvals",
            description: "Show pending approvals",
        },
        CommandItem {
            command: "/context add <path>",
            description: "Attach a context file chip to Charm input",
        },
        CommandItem {
            command: "/context clear",
            description: "Clear Charm input context chips",
        },
        CommandItem {
            command: "/mcp",
            description: "Show MCP servers and tool inventory",
        },
        CommandItem {
            command: "/mcp refresh",
            description: "Probe MCP servers and refresh live tool inventory",
        },
        CommandItem {
            command: "/mcp call <server> <tool> [json]",
            description: "Invoke an MCP tool with optional JSON arguments",
        },
        CommandItem {
            command: "/lsp",
            description: "Show LSP roots and diagnostics summary",
        },
        CommandItem {
            command: "/lsp refresh",
            description: "Run workspace diagnostics refresh and update cache",
        },
        CommandItem {
            command: "/lsp diagnostics",
            description: "Show cached diagnostics in the transcript",
        },
        CommandItem {
            command: "/lsp symbols",
            description: "Show indexed symbol jump targets",
        },
    ]
}

pub fn slash_suggestions(input: &str) -> Vec<CommandItem> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return Vec::new();
    }
    command_catalog()
        .into_iter()
        .filter(|item| item.command.starts_with(trimmed) || trimmed == "/")
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Palette,
    Sessions,
    ModelSwitcher,
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
}

impl Default for SessionApp {
    fn default() -> Self {
        let theme = Theme::default();
        Self {
            session_id: String::new(),
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
        match event {
            RuntimeEvent::SessionLifecycle {
                session_id,
                lifecycle,
                summary,
            } => {
                self.session_id = session_id;
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
                    if let Some(last_line) = self.transcript.last_mut() {
                        if last_line.spans.is_empty() {
                            let icon = match role.as_str() {
                                "assistant" => "✦",
                                _ => "·",
                            };
                            let styled_spans = style_line(&content, color, &self.theme);
                            let mut line_spans = vec![Span::styled(
                                format!(" {icon} "),
                                Style::default().fg(color).add_modifier(Modifier::BOLD),
                            )];
                            line_spans.extend(styled_spans);
                            *last_line = Line::from(line_spans);
                        } else {
                            let icon_span = last_line.spans.first().cloned();
                            let content_text: String = last_line
                                .spans
                                .iter()
                                .skip(1)
                                .map(|s| s.content.clone())
                                .collect();
                            let new_content = format!("{}{}", content_text, content);
                            let styled_spans = style_line(&new_content, color, &self.theme);
                            let mut new_spans = Vec::new();
                            if let Some(icon) = icon_span {
                                new_spans.push(icon);
                            }
                            new_spans.extend(styled_spans);
                            *last_line = Line::from(new_spans);
                        }
                    } else {
                        let icon = match role.as_str() {
                            "assistant" => "✦",
                            _ => "·",
                        };
                        let styled_spans = style_line(&content, color, &self.theme);
                        let mut line_spans = vec![Span::styled(
                            format!(" {icon} "),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        )];
                        line_spans.extend(styled_spans);
                        self.transcript.push(Line::from(line_spans));
                    }
                    self.auto_scroll = true;
                }
            }
            RuntimeEvent::StreamDone { model: _ } => {
                self.processing = false;
                if let Some(last_line) = self.transcript.last_mut() {
                    last_line.spans.push(Span::raw(""));
                }
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
        }
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
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = SessionApp::default();
    app.apply_events(initial_events);
    rt.block_on(app.fetch_available_models());

    let (bridge, _runtime_handle) = EventBridge::spawn_runtime(runtime, rt);
    app.input_sender = Some(bridge.input_tx.clone());
    let result = run_loop(&mut terminal, &mut app, bridge);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
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
            Event::Mouse(_) => {}
            Event::Resize(_, _) => {
                app.auto_scroll = true;
            }
            _ => {}
        }
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
    }

    fn scroll_down(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(amount);
    }
}

fn handle_key_event(key: KeyEvent, app: &mut SessionApp) -> anyhow::Result<bool> {
    if app.overlay != Overlay::None {
        match key.code {
            KeyCode::Esc => {
                app.overlay = Overlay::None;
                return Ok(false);
            }
            KeyCode::Up => {
                app.overlay_index = app.overlay_index.saturating_sub(1);
                return Ok(false);
            }
            KeyCode::Down => {
                let items_len = match app.overlay {
                    Overlay::Palette => app.palette_items().len(),
                    Overlay::Sessions => app.sessions.len(),
                    Overlay::ModelSwitcher => app.available_models.len(),
                    Overlay::None => 0,
                };
                let last = items_len.saturating_sub(1);
                app.overlay_index = (app.overlay_index + 1).min(last);
                return Ok(false);
            }
            KeyCode::Enter => {
                match app.overlay {
                    Overlay::Palette => {
                        if let Some(item) = app.palette_items().get(app.overlay_index) {
                            app.input.buffer = item.command.to_string();
                            app.input.cursor = app.input.buffer.len();
                        }
                    }
                    Overlay::Sessions => {
                        if let Some(session) = app.sessions.get(app.overlay_index) {
                            app.input.buffer = format!("/session {}", session.id);
                            app.input.cursor = app.input.buffer.len();
                        }
                    }
                    Overlay::ModelSwitcher => {
                        if let Some(model) = app.available_models.get(app.overlay_index) {
                            app.input.buffer = format!("/model {}", model.model_id);
                            app.input.cursor = app.input.buffer.len();
                        }
                    }
                    Overlay::None => {}
                }
                app.overlay = Overlay::None;
                return Ok(false);
            }
            _ => {}
        }
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
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
                app.overlay = if app.overlay == Overlay::None {
                    Overlay::Palette
                } else if app.overlay == Overlay::Palette {
                    Overlay::None
                } else {
                    app.overlay
                };
                return Ok(false);
            }
            KeyCode::Char('l') => {
                app.refresh_sessions();
                app.overlay = if app.overlay == Overlay::None {
                    Overlay::Sessions
                } else if app.overlay == Overlay::Sessions {
                    Overlay::None
                } else {
                    app.overlay
                };
                return Ok(false);
            }
            KeyCode::Char('m') => {
                app.overlay = if app.overlay == Overlay::None {
                    Overlay::ModelSwitcher
                } else if app.overlay == Overlay::ModelSwitcher {
                    Overlay::None
                } else {
                    app.overlay
                };
                return Ok(false);
            }
            KeyCode::Char('n') => {
                app.input.buffer = "/new".to_string();
                app.input.cursor = app.input.buffer.len();
                if let Some(input) = app.input.submit() {
                    if let Some(sender) = &app.input_sender {
                        let _ = sender.send(input);
                    }
                }
                return Ok(false);
            }
            _ => {}
        }
    }

    if key.modifiers.contains(KeyModifiers::SHIFT) {
        match key.code {
            KeyCode::Up => {
                app.scroll_up(3);
                return Ok(false);
            }
            KeyCode::Down => {
                app.scroll_down(3);
                return Ok(false);
            }
            KeyCode::BackTab => {
                app.current_intent = match app.current_intent {
                    RouterIntent::Explore => RouterIntent::Verify,
                    RouterIntent::Plan => RouterIntent::Explore,
                    RouterIntent::Implement => RouterIntent::Plan,
                    RouterIntent::Verify => RouterIntent::Implement,
                };
                app.transcript.push(Line::from(vec![
                    Span::styled(" ◈ ", Style::default().fg(app.theme.role_router)),
                    Span::styled(
                        format!("{:?}", app.current_intent),
                        Style::default().fg(app.theme.warning),
                    ),
                    Span::styled(" mode", Style::default().fg(app.theme.dim)),
                ]));
                app.auto_scroll = true;
                return Ok(false);
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Esc => Ok(true),
        KeyCode::Tab => {
            if app.overlay == Overlay::Palette {
                let last = app.palette_items().len().saturating_sub(1);
                app.overlay_index = (app.overlay_index + 1).min(last);
            } else {
                app.current_intent = match app.current_intent {
                    RouterIntent::Explore => RouterIntent::Plan,
                    RouterIntent::Plan => RouterIntent::Implement,
                    RouterIntent::Implement => RouterIntent::Verify,
                    RouterIntent::Verify => RouterIntent::Explore,
                };
                app.transcript.push(Line::from(vec![
                    Span::styled(" ◈ ", Style::default().fg(app.theme.role_router)),
                    Span::styled(
                        format!("{:?}", app.current_intent),
                        Style::default().fg(app.theme.warning),
                    ),
                    Span::styled(" mode", Style::default().fg(app.theme.dim)),
                ]));
                app.auto_scroll = true;
            }
            Ok(false)
        }
        KeyCode::PageUp => {
            app.scroll_up(10);
            Ok(false)
        }
        KeyCode::PageDown => {
            app.scroll_down(10);
            Ok(false)
        }
        KeyCode::Up if app.input.is_empty() && app.overlay == Overlay::None => {
            app.input.history_up();
            Ok(false)
        }
        KeyCode::Down if app.input.is_empty() && app.overlay == Overlay::None => {
            app.input.history_down();
            Ok(false)
        }
        KeyCode::Left => {
            if app.overlay == Overlay::None {
                app.input.move_left();
            }
            Ok(false)
        }
        KeyCode::Right => {
            if app.overlay == Overlay::None {
                app.input.move_right();
            }
            Ok(false)
        }
        KeyCode::Home => {
            if app.overlay == Overlay::None {
                app.input.move_home();
            }
            Ok(false)
        }
        KeyCode::End => {
            if app.overlay == Overlay::None {
                app.input.move_end();
            }
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
            if let Some(input) = app.input.submit() {
                app.processing = true;
                if let Some(sender) = &app.input_sender {
                    let _ = sender.send(input);
                }
            }
            Ok(false)
        }
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.input.delete_word();
            Ok(false)
        }
        KeyCode::Char(ch) => {
            app.input.insert(ch);
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn render(frame: &mut ratatui::Frame<'_>, app: &mut SessionApp) {
    let theme = &app.theme;
    let bg = theme.bg_secondary;

    frame.render_widget(
        Block::default().style(Style::default().bg(bg)),
        frame.area(),
    );

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(3),
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
    render_transcript(frame, app, main[cursor]);
    cursor += 1;
    if app.show_right_dock {
        render_right_dock(frame, app, main[cursor]);
    }
    render_status(frame, app, outer[1]);
    render_composer(frame, app, outer[2]);

    if app.overlay != Overlay::None {
        match app.overlay {
            Overlay::Palette => render_palette(frame, app),
            Overlay::Sessions => render_sessions(frame, app),
            Overlay::ModelSwitcher => render_model_switcher(frame, app),
            Overlay::None => {}
        }
    }
}

fn render_transcript(frame: &mut ratatui::Frame<'_>, app: &mut SessionApp, area: Rect) {
    let theme = &app.theme;

    let scroll = if app.auto_scroll {
        let total_lines = app.transcript.len() as u16;
        total_lines.saturating_sub(area.height.saturating_sub(2))
    } else {
        app.scroll_offset
    };

    let title = if app.processing {
        let spinner = app.spinner.tick();
        format!(" {} {}", spinner, app.session_id)
    } else {
        format!(" {}", app.session_id)
    };

    let paragraph = Paragraph::new(Text::from(app.transcript.clone()))
        .block(
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
                .style(Style::default().bg(theme.bg_primary)),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(paragraph, area);
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

    let jobs_lines = if app.background_jobs.is_empty() {
        vec![Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("No background jobs", Style::default().fg(theme.dim)),
        ])]
    } else {
        app.background_jobs
            .iter()
            .map(|job| {
                let icon = match job.status {
                    crate::runtime::types::BackgroundJobStatus::Running => "◉",
                    crate::runtime::types::BackgroundJobStatus::Completed => "✓",
                    crate::runtime::types::BackgroundJobStatus::Failed => "✗",
                };
                let icon_color = match job.status {
                    crate::runtime::types::BackgroundJobStatus::Running => theme.warning,
                    crate::runtime::types::BackgroundJobStatus::Completed => theme.success,
                    crate::runtime::types::BackgroundJobStatus::Failed => theme.error,
                };
                Line::from(vec![
                    Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
                    Span::styled(job.title.clone(), Style::default().fg(theme.text_secondary)),
                ])
            })
            .collect()
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

    let intent_icon = match app.current_intent {
        RouterIntent::Explore => "◈",
        RouterIntent::Plan => "◈",
        RouterIntent::Implement => "◈",
        RouterIntent::Verify => "◈",
    };

    let mut spans = vec![
        Span::styled(
            format!(" {intent_icon} "),
            Style::default().fg(theme.accent),
        ),
        Span::styled(
            format!("{:?}", app.current_intent),
            Style::default().fg(theme.status_label),
        ),
        Span::styled(" │ ", Style::default().fg(theme.dim)),
        Span::styled(
            format!("Autonomy {:?}", app.autonomy),
            Style::default().fg(theme.text_secondary),
        ),
    ];

    if pending > 0 {
        spans.push(Span::styled(" │ ", Style::default().fg(theme.dim)));
        spans.push(Span::styled(
            format!("⚠ {} approvals", pending),
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
        ));
    }

    if !app.background_jobs.is_empty() {
        spans.push(Span::styled(" │ ", Style::default().fg(theme.dim)));
        spans.push(Span::styled(
            format!("{} jobs", app.background_jobs.len()),
            Style::default().fg(theme.warning),
        ));
    }

    if app.processing {
        spans.push(Span::styled(" │ ", Style::default().fg(theme.dim)));
        spans.push(Span::styled(
            app.spinner.tick().to_string(),
            Style::default().fg(theme.accent),
        ));
    }

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

    let display_text = if input_text.is_empty() {
        format!(
            "\n  Type a message...\n{}",
            if suggestions.is_empty() {
                String::new()
            } else {
                suggestions
                    .iter()
                    .map(|s| format!("  {} {}", s.command, s.description))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        )
    } else {
        let ctx = if app.context_items.is_empty() {
            String::new()
        } else {
            format!("{} │ ", app.context_items.join(" · "))
        };
        let suggestion_text = if suggestions.is_empty() {
            String::new()
        } else {
            let suggestion_strs: Vec<String> = suggestions
                .iter()
                .map(|s| format!("{} {}", s.command, s.description))
                .collect();
            format!("\n{}", suggestion_strs.join("\n"))
        };
        format!(
            "{}{}\n{}",
            ctx,
            input_text,
            if suggestion_text.is_empty() {
                String::new()
            } else {
                suggestion_text.replace('\n', "\n")
            }
        )
    };

    let border_color = if app.processing {
        theme.accent
    } else if !input_text.is_empty() {
        theme.border_focused
    } else {
        theme.border
    };

    let composer_block = Block::default()
        .title(Span::styled(
            " Charm",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(theme.bg_composer));

    let paragraph = Paragraph::new(display_text)
        .block(composer_block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);

    if app.overlay == Overlay::None && !input_text.is_empty() && app.cursor_visible {
        let ctx_width: usize = if app.context_items.is_empty() {
            0
        } else {
            UnicodeWidthStr::width(format!("{} │ ", app.context_items.join(" · ")).as_str())
        };
        let cursor_display = app.input.display_cursor_width();
        let cursor_x = area.x + 1 + (ctx_width + cursor_display) as u16;
        let cursor_y = area.y + 1;
        frame.set_cursor_position((
            cursor_x.min(area.x + area.width.saturating_sub(2)),
            cursor_y,
        ));
    }
}

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
