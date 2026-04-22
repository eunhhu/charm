use crate::runtime::session_runtime::SessionRuntime;
use crate::runtime::types::{
    ApprovalRequest, ApprovalStatus, AutonomyLevel, BackgroundJob, LspSnapshot, McpSnapshot,
    RouterIntent, RuntimeEvent, SessionLifecycle, WorkspacePreflight,
};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use std::io::{self, Stdout};
use std::time::Duration;

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
            description: "Attach a context file chip to the composer",
        },
        CommandItem {
            command: "/context clear",
            description: "Clear composer context chips",
        },
        CommandItem {
            command: "/mcp",
            description: "Show MCP servers and tool inventory",
        },
        CommandItem {
            command: "/lsp",
            description: "Show LSP roots and diagnostics summary",
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

pub struct SessionApp {
    pub session_id: String,
    pub transcript: Vec<Line<'static>>,
    pub composer: String,
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
    pub palette_open: bool,
    pub palette_index: usize,
    pub lifecycle: SessionLifecycle,
}

impl Default for SessionApp {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            transcript: Vec::new(),
            composer: String::new(),
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
            palette_open: false,
            palette_index: 0,
            lifecycle: SessionLifecycle::Idle,
        }
    }
}

impl SessionApp {
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
                    Span::styled("[session] ", Style::default().fg(Color::Cyan)),
                    Span::raw(summary),
                ]));
            }
            RuntimeEvent::MessageDelta { role, content } => {
                self.transcript.push(Line::from(vec![
                    Span::styled(format!("[{role}] "), Style::default().fg(Color::Green)),
                    Span::raw(content),
                ]));
            }
            RuntimeEvent::RouterStateChanged { intent, source } => {
                self.current_intent = intent;
                self.transcript.push(Line::from(vec![
                    Span::styled("[router] ", Style::default().fg(Color::Yellow)),
                    Span::raw(format!("{intent:?} via {source}")),
                ]));
            }
            RuntimeEvent::ToolCallStarted { execution } => {
                self.transcript.push(Line::from(vec![
                    Span::styled("[tool] ", Style::default().fg(Color::Blue)),
                    Span::raw(format!("start {}", execution.summary)),
                ]));
            }
            RuntimeEvent::ToolCallFinished { execution, result } => {
                self.transcript.push(Line::from(vec![
                    Span::styled("[tool] ", Style::default().fg(Color::Blue)),
                    Span::raw(format!(
                        "done {} -> {}",
                        execution.tool_name,
                        result.output.lines().next().unwrap_or("ok")
                    )),
                ]));
            }
            RuntimeEvent::ApprovalRequested { approval } => {
                self.pending_approvals.push(approval.clone());
                self.transcript.push(Line::from(vec![
                    Span::styled("[approval] ", Style::default().fg(Color::Red)),
                    Span::raw(format!("pending {}", approval.summary)),
                ]));
            }
            RuntimeEvent::ApprovalResolved { approval } => {
                if let Some(existing) = self
                    .pending_approvals
                    .iter_mut()
                    .find(|item| item.id == approval.id)
                {
                    *existing = approval.clone();
                }
                self.transcript.push(Line::from(vec![
                    Span::styled("[approval] ", Style::default().fg(Color::Red)),
                    Span::raw(format!("{:?} {}", approval.status, approval.summary)),
                ]));
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

    pub fn palette_items(&self) -> Vec<CommandItem> {
        command_catalog()
    }

    pub fn suggestion_items(&self) -> Vec<CommandItem> {
        slash_suggestions(&self.composer)
    }
}

pub fn run_session_tui(
    runtime: &mut SessionRuntime,
    rt: &tokio::runtime::Runtime,
    initial_events: Vec<RuntimeEvent>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = SessionApp::default();
    app.apply_events(initial_events);

    let result = run_loop(&mut terminal, &mut app, runtime, rt);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut SessionApp,
    runtime: &mut SessionRuntime,
    rt: &tokio::runtime::Runtime,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| render(frame, app))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };

        if handle_key_event(key, app, runtime, rt)? {
            return Ok(());
        }
    }
}

fn handle_key_event(
    key: KeyEvent,
    app: &mut SessionApp,
    runtime: &mut SessionRuntime,
    rt: &tokio::runtime::Runtime,
) -> anyhow::Result<bool> {
    if app.palette_open {
        match key.code {
            KeyCode::Esc => {
                app.palette_open = false;
                return Ok(false);
            }
            KeyCode::Up => {
                app.palette_index = app.palette_index.saturating_sub(1);
                return Ok(false);
            }
            KeyCode::Down => {
                let last = app.palette_items().len().saturating_sub(1);
                app.palette_index = (app.palette_index + 1).min(last);
                return Ok(false);
            }
            KeyCode::Enter => {
                if let Some(item) = app.palette_items().get(app.palette_index) {
                    app.composer = item.command.to_string();
                }
                app.palette_open = false;
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
                app.palette_open = !app.palette_open;
                return Ok(false);
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Esc => Ok(true),
        KeyCode::Backspace => {
            app.composer.pop();
            Ok(false)
        }
        KeyCode::Enter => {
            let input = std::mem::take(&mut app.composer);
            if !input.trim().is_empty() {
                let events = rt.block_on(runtime.submit_input(&input))?;
                app.apply_events(events);
                app.context_items = runtime.snapshot().composer.context_items.clone();
            }
            Ok(false)
        }
        KeyCode::Char(ch) => {
            app.composer.push(ch);
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn render(frame: &mut ratatui::Frame<'_>, app: &SessionApp) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(1),
            Constraint::Length(4),
        ])
        .split(frame.area());

    let mut horizontal = Vec::new();
    if app.show_left_dock {
        horizontal.push(Constraint::Length(30));
    }
    horizontal.push(Constraint::Min(40));
    if app.show_right_dock {
        horizontal.push(Constraint::Length(36));
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

    if app.palette_open {
        render_palette(frame, app);
    }
}

fn render_left_dock(frame: &mut ratatui::Frame<'_>, app: &SessionApp, area: Rect) {
    let preflight = vec![
        Line::from(format!("Session: {}", app.session_id)),
        Line::from(format!("Branch: {}", app.preflight.branch)),
        Line::from(format!("Dirty: {}", app.preflight.dirty_files.len())),
        Line::from(""),
        Line::from("Suggested"),
    ]
    .into_iter()
    .chain(
        app.preflight
            .suggested_actions
            .iter()
            .map(|item| Line::from(format!("• {item}"))),
    )
    .chain(std::iter::once(Line::from("")))
    .chain(std::iter::once(Line::from("Context")))
    .chain(
        app.context_items
            .iter()
            .map(|item| Line::from(format!("• {item}"))),
    )
    .collect::<Vec<_>>();

    let paragraph = Paragraph::new(Text::from(preflight))
        .block(Block::default().title("Workspace").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_transcript(frame: &mut ratatui::Frame<'_>, app: &SessionApp, area: Rect) {
    let paragraph = Paragraph::new(Text::from(app.transcript.clone()))
        .block(Block::default().title("Transcript").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_right_dock(frame: &mut ratatui::Frame<'_>, app: &SessionApp, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Min(6),
        ])
        .split(area);

    let lsp_lines = vec![
        Line::from(format!("Ready: {}", app.lsp.ready)),
        Line::from(format!("Roots: {}", app.lsp.active_roots.join(", "))),
        Line::from(format!("Diagnostics: {}", app.lsp.diagnostics.len())),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(lsp_lines))
            .block(Block::default().title("LSP").borders(Borders::ALL)),
        chunks[0],
    );

    let mcp_lines = vec![
        Line::from(format!("Ready: {}", app.mcp.ready)),
        Line::from(format!(
            "Servers: {}",
            app.mcp
                .servers
                .iter()
                .map(|server| server.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Line::from(format!("Tools: {}", app.mcp.tools.len())),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(mcp_lines))
            .block(Block::default().title("MCP").borders(Borders::ALL)),
        chunks[1],
    );

    let approval_lines = if app.pending_approvals.is_empty() {
        vec![Line::from("No pending approvals")]
    } else {
        app.pending_approvals
            .iter()
            .filter(|approval| approval.status == ApprovalStatus::Pending)
            .map(|approval| Line::from(format!("{} {}", approval.id, approval.tool_name)))
            .collect::<Vec<_>>()
    };
    frame.render_widget(
        Paragraph::new(Text::from(approval_lines))
            .block(Block::default().title("Approvals").borders(Borders::ALL)),
        chunks[2],
    );

    let jobs_lines = if app.background_jobs.is_empty() {
        vec![Line::from("No background jobs")]
    } else {
        app.background_jobs
            .iter()
            .map(|job| Line::from(format!("{} {:?}", job.title, job.status)))
            .collect::<Vec<_>>()
    };
    frame.render_widget(
        Paragraph::new(Text::from(jobs_lines))
            .block(Block::default().title("Background").borders(Borders::ALL)),
        chunks[3],
    );
}

fn render_status(frame: &mut ratatui::Frame<'_>, app: &SessionApp, area: Rect) {
    let status = Line::from(vec![
        Span::styled("Intent ", Style::default().fg(Color::Yellow)),
        Span::raw(format!("{:?}", app.current_intent)),
        Span::raw("  "),
        Span::styled("Autonomy ", Style::default().fg(Color::Yellow)),
        Span::raw(format!("{:?}", app.autonomy)),
        Span::raw("  "),
        Span::styled("Approvals ", Style::default().fg(Color::Yellow)),
        Span::raw(
            app.pending_approvals
                .iter()
                .filter(|approval| approval.status == ApprovalStatus::Pending)
                .count()
                .to_string(),
        ),
        Span::raw("  "),
        Span::styled("Jobs ", Style::default().fg(Color::Yellow)),
        Span::raw(app.background_jobs.len().to_string()),
    ]);
    frame.render_widget(Paragraph::new(status).alignment(Alignment::Left), area);
}

fn render_composer(frame: &mut ratatui::Frame<'_>, app: &SessionApp, area: Rect) {
    let suggestions = app
        .suggestion_items()
        .into_iter()
        .map(|item| format!("{} {}", item.command, item.description))
        .collect::<Vec<_>>()
        .join("\n");
    let chips = if app.context_items.is_empty() {
        String::new()
    } else {
        format!("Context: {}\n", app.context_items.join(" | "))
    };
    let paragraph = Paragraph::new(format!("{chips}> {}\n{}", app.composer, suggestions))
        .block(Block::default().title("Composer").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_palette(frame: &mut ratatui::Frame<'_>, app: &SessionApp) {
    let area = centered_rect(60, 50, frame.area());
    let items = app
        .palette_items()
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let style = if index == app.palette_index {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(format!("{} {}", item.command, item.description)).style(style)
        })
        .collect::<Vec<_>>();

    frame.render_widget(Clear, area);
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title("Command Palette")
                .borders(Borders::ALL),
        ),
        area,
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
                .any(|line| line.to_string().contains("approval"))
        );
    }
}
