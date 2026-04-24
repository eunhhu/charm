//! Generic select-dialog widget inspired by OpenCode's `DialogSelect`.
//!
//! Features:
//! - Fuzzy substring filtering over title + category.
//! - Category grouping with headers when `flat == false`.
//! - Keyboard navigation (up/down, pageup/pagedown, home/end).
//! - Mouse hover + click (caller routes mouse events).
//! - Optional keybind hint bar at the bottom.
//! - Input mode tracking so mouse-move doesn't steal selection from keyboard.
//!
//! The widget is rendered via [`render_dialog_select`] and mutates a
//! [`DialogSelectState`] owned by the TUI. Filtering is done at render time
//! from the full option list, so callers don't need to pre-filter.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::tui::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Keyboard,
    Mouse,
}

#[derive(Debug, Clone)]
pub struct DialogOption {
    /// Stable key returned to the caller when selected.
    pub value: String,
    pub title: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub footer: Option<String>,
    /// Optional gutter marker (used for "current"/"favorite" dots). Rendered
    /// to the left of the title.
    pub marker: Option<String>,
    /// Colored marker foreground; if `None`, the theme accent is used.
    pub marker_style: Option<Style>,
    /// Dim the row (e.g. disconnected providers).
    pub disabled: bool,
}

impl DialogOption {
    pub fn new(value: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            title: title.into(),
            description: None,
            category: None,
            footer: None,
            marker: None,
            marker_style: None,
            disabled: false,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    pub fn marker(mut self, marker: impl Into<String>, style: Style) -> Self {
        self.marker = Some(marker.into());
        self.marker_style = Some(style);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

#[derive(Debug, Clone)]
pub struct KeybindHint {
    pub keys: String,
    pub title: String,
}

impl KeybindHint {
    pub fn new(keys: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            keys: keys.into(),
            title: title.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DialogSelectState {
    /// Current filter query (fuzzy).
    pub filter: String,
    /// Cursor inside the filter input.
    pub filter_cursor: usize,
    /// Index into the flattened filtered list.
    pub selected: usize,
    /// Scroll offset for the option list.
    pub scroll: u16,
    pub input_mode: InputMode,
}

impl Default for DialogSelectState {
    fn default() -> Self {
        Self {
            filter: String::new(),
            filter_cursor: 0,
            selected: 0,
            scroll: 0,
            input_mode: InputMode::Keyboard,
        }
    }
}

impl DialogSelectState {
    pub fn reset(&mut self) {
        self.filter.clear();
        self.filter_cursor = 0;
        self.selected = 0;
        self.scroll = 0;
        self.input_mode = InputMode::Keyboard;
    }

    pub fn insert_char(&mut self, c: char) {
        self.filter.insert(self.filter_cursor, c);
        self.filter_cursor += c.len_utf8();
        self.selected = 0;
        self.scroll = 0;
        self.input_mode = InputMode::Keyboard;
    }

    pub fn backspace(&mut self) {
        if self.filter_cursor == 0 {
            return;
        }
        let mut prev = self.filter_cursor - 1;
        while prev > 0 && !self.filter.is_char_boundary(prev) {
            prev -= 1;
        }
        self.filter.drain(prev..self.filter_cursor);
        self.filter_cursor = prev;
        self.selected = 0;
        self.scroll = 0;
        self.input_mode = InputMode::Keyboard;
    }

    pub fn move_cursor_left(&mut self) {
        if self.filter_cursor == 0 {
            return;
        }
        let mut prev = self.filter_cursor - 1;
        while prev > 0 && !self.filter.is_char_boundary(prev) {
            prev -= 1;
        }
        self.filter_cursor = prev;
    }

    pub fn move_cursor_right(&mut self) {
        if self.filter_cursor >= self.filter.len() {
            return;
        }
        let mut next = self.filter_cursor + 1;
        while next < self.filter.len() && !self.filter.is_char_boundary(next) {
            next += 1;
        }
        self.filter_cursor = next;
    }

    pub fn move_selection(&mut self, delta: i32, total: usize) {
        if total == 0 {
            self.selected = 0;
            return;
        }
        let total = total as i32;
        let next = ((self.selected as i32 + delta) % total + total) % total;
        self.selected = next as usize;
        self.input_mode = InputMode::Keyboard;
    }
}

/// Score a single option against a query. Higher is better. Returns `None`
/// if the query does not fuzzy-match.
pub fn fuzzy_score(query: &str, title: &str, category: Option<&str>) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q = query.to_lowercase();
    let hay = title.to_lowercase();
    let cat = category.map(|c| c.to_lowercase()).unwrap_or_default();

    // Prefix match is best.
    if hay.starts_with(&q) {
        return Some(1000 - hay.len() as i32);
    }
    // Substring match in title.
    if let Some(pos) = hay.find(&q) {
        return Some(500 - pos as i32);
    }
    // Category substring match.
    if !cat.is_empty() && cat.contains(&q) {
        return Some(200);
    }
    // Scattered characters in order (very loose fuzzy).
    let mut hay_iter = hay.chars();
    let mut matched = 0i32;
    for qc in q.chars() {
        let mut found = false;
        for hc in hay_iter.by_ref() {
            if hc == qc {
                matched += 1;
                found = true;
                break;
            }
        }
        if !found {
            return None;
        }
    }
    Some(matched)
}

/// Apply the current filter and return the flattened list of matching
/// options along with their original indices. Category headers are inserted
/// as `Row::Header` entries (only when `flatten == false` and filter is
/// empty).
pub enum Row<'a> {
    Header(&'a str),
    Option(usize),
    Spacer,
}

pub fn filter_and_flatten<'a>(
    options: &'a [DialogOption],
    state: &DialogSelectState,
    flat: bool,
) -> (Vec<Row<'a>>, Vec<usize>) {
    // Filter first.
    let mut scored: Vec<(usize, i32)> = options
        .iter()
        .enumerate()
        .filter(|(_, opt)| !opt.disabled || !state.filter.is_empty())
        .filter_map(|(idx, opt)| {
            fuzzy_score(&state.filter, &opt.title, opt.category.as_deref()).map(|s| (idx, s))
        })
        .collect();

    if !state.filter.is_empty() {
        scored.sort_by(|a, b| b.1.cmp(&a.1));
    }

    let filtered_indices: Vec<usize> = scored.iter().map(|(i, _)| *i).collect();

    // Build rows.
    let show_headers = !flat && state.filter.is_empty();
    let mut rows: Vec<Row<'a>> = Vec::new();
    if !show_headers {
        for idx in &filtered_indices {
            rows.push(Row::Option(*idx));
        }
    } else {
        let mut current_category: Option<&str> = None;
        let mut first = true;
        for idx in &filtered_indices {
            let cat = options[*idx].category.as_deref();
            if cat != current_category {
                if !first {
                    rows.push(Row::Spacer);
                }
                if let Some(c) = cat {
                    rows.push(Row::Header(c));
                }
                current_category = cat;
                first = false;
            }
            rows.push(Row::Option(*idx));
        }
    }

    (rows, filtered_indices)
}

pub struct DialogSelectProps<'a> {
    pub title: &'a str,
    pub placeholder: &'a str,
    pub options: &'a [DialogOption],
    pub state: &'a DialogSelectState,
    pub flat: bool,
    pub keybinds: &'a [KeybindHint],
    pub width_pct: u16,
    pub height_pct: u16,
    /// Value of the currently "pinned" option — rendered with a filled dot.
    pub current: Option<&'a str>,
}

pub struct DialogSelectLayout {
    pub outer: Rect,
    pub filter: Rect,
    pub list: Rect,
    pub hints: Rect,
    /// Y-positions (relative to `outer`) of option rows, paired with their
    /// option index. Used for mouse click routing.
    pub option_y_map: Vec<(u16, usize)>,
}

pub fn render_dialog_select(
    frame: &mut ratatui::Frame<'_>,
    theme: &Theme,
    props: &DialogSelectProps<'_>,
) -> DialogSelectLayout {
    let area = centered_rect(props.width_pct, props.height_pct, frame.area());
    frame.render_widget(Clear, area);

    let hints_height = if props.keybinds.is_empty() { 0 } else { 2 };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),            // filter row
            Constraint::Min(3),               // list
            Constraint::Length(hints_height), // hints
        ])
        .split(area);

    let filter_area = layout[0];
    let list_area = layout[1];
    let hints_area = layout[2];

    // Container border around the whole thing.
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border_focused))
            .title(Span::styled(
                format!(" {} ", props.title),
                Style::default()
                    .fg(theme.dock_title)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(theme.bg_secondary)),
        area,
    );

    // ===== Filter row =====
    let filter_display = if props.state.filter.is_empty() {
        Line::from(vec![
            Span::styled(" ⌕ ", Style::default().fg(theme.accent)),
            Span::styled(
                props.placeholder.to_string(),
                Style::default().fg(theme.dim),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ⌕ ", Style::default().fg(theme.accent)),
            Span::styled(
                props.state.filter.clone(),
                Style::default().fg(theme.text_primary),
            ),
        ])
    };

    let inner_filter = filter_area.inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 1,
    });
    frame.render_widget(
        Paragraph::new(Text::from(vec![filter_display]))
            .style(Style::default().bg(theme.bg_secondary)),
        inner_filter,
    );

    // ===== Option list =====
    let (rows, _filtered) = filter_and_flatten(props.options, props.state, props.flat);
    let inner_list = list_area.inner(ratatui::layout::Margin {
        horizontal: 2,
        vertical: 0,
    });

    // Build displayable lines.
    let mut display_lines: Vec<(Line<'static>, Option<usize>)> = Vec::new();
    for row in &rows {
        match row {
            Row::Header(title) => {
                display_lines.push((
                    Line::from(Span::styled(
                        format!(" {title}"),
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    )),
                    None,
                ));
            }
            Row::Spacer => {
                display_lines.push((Line::from(""), None));
            }
            Row::Option(idx) => {
                let opt = &props.options[*idx];
                let selected_value = rows
                    .iter()
                    .filter_map(|r| match r {
                        Row::Option(i) => Some(*i),
                        _ => None,
                    })
                    .nth(props.state.selected);
                let active = selected_value == Some(*idx);
                let is_current = props.current.map(|c| c == opt.value).unwrap_or(false);

                let (fg, bg) = if active {
                    (theme.palette_selected_fg, theme.palette_selected_bg)
                } else if opt.disabled {
                    (theme.dim, theme.bg_secondary)
                } else if is_current {
                    (theme.accent, theme.bg_secondary)
                } else {
                    (theme.text_primary, theme.bg_secondary)
                };

                let mut spans: Vec<Span> = Vec::new();
                // Gutter: marker or current indicator.
                let gutter = if is_current {
                    Span::styled(" ● ", Style::default().fg(fg).bg(bg))
                } else if let Some(marker) = &opt.marker {
                    let m_style = opt
                        .marker_style
                        .map(|s| s.bg(bg))
                        .unwrap_or_else(|| Style::default().fg(theme.accent).bg(bg));
                    Span::styled(format!(" {marker} "), m_style)
                } else {
                    Span::styled("   ", Style::default().bg(bg))
                };
                spans.push(gutter);

                // Title.
                let title_text =
                    truncate_str(&opt.title, inner_list.width.saturating_sub(12) as usize);
                spans.push(Span::styled(
                    title_text,
                    Style::default().fg(fg).bg(bg).add_modifier(if active {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
                ));

                // Description.
                if let Some(desc) = &opt.description {
                    spans.push(Span::styled(
                        format!(" {desc}"),
                        Style::default()
                            .fg(if active { fg } else { theme.dim })
                            .bg(bg),
                    ));
                }

                // Footer — right aligned, so we pad with spaces.
                let content_width: usize = spans
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum();
                if let Some(footer) = &opt.footer {
                    let footer_w = UnicodeWidthStr::width(footer.as_str());
                    let pad =
                        (inner_list.width as usize).saturating_sub(content_width + footer_w + 1);
                    if pad > 0 {
                        spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
                    }
                    spans.push(Span::styled(
                        footer.clone(),
                        Style::default()
                            .fg(if active { fg } else { theme.dim })
                            .bg(bg),
                    ));
                } else if active {
                    let pad = (inner_list.width as usize).saturating_sub(content_width);
                    if pad > 0 {
                        spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
                    }
                }

                display_lines.push((Line::from(spans), Some(*idx)));
            }
        }
    }

    // Scrolling: clamp so the selected row is visible.
    let list_height = inner_list.height as usize;
    // Find y of selected option in display_lines.
    let selected_display_y = display_lines
        .iter()
        .position(|(_, opt_idx)| {
            opt_idx.is_some() && {
                let pos_in_options = display_lines
                    .iter()
                    .filter(|(_, i)| i.is_some())
                    .position(|(_, i)| i.map(|x| x) == opt_idx.map(|x| x));
                pos_in_options == Some(props.state.selected)
            }
        })
        .unwrap_or(0);

    let state_scroll = props.state.scroll as usize;
    let mut effective_scroll = state_scroll;
    if selected_display_y < state_scroll {
        effective_scroll = selected_display_y;
    } else if selected_display_y >= state_scroll + list_height {
        effective_scroll = selected_display_y + 1 - list_height.max(1);
    }

    let visible: Vec<Line> = display_lines
        .iter()
        .skip(effective_scroll)
        .take(list_height)
        .map(|(line, _)| line.clone())
        .collect();

    frame.render_widget(
        Paragraph::new(Text::from(visible)).style(Style::default().bg(theme.bg_secondary)),
        inner_list,
    );

    // Option y-map: translate display row -> frame row.
    let mut option_y_map: Vec<(u16, usize)> = Vec::new();
    for (i, (_, opt_idx)) in display_lines
        .iter()
        .enumerate()
        .skip(effective_scroll)
        .take(list_height)
    {
        if let Some(idx) = opt_idx {
            let y = inner_list.y + (i - effective_scroll) as u16;
            option_y_map.push((y, *idx));
        }
    }

    // ===== Keybind hints =====
    if hints_height > 0 && !props.keybinds.is_empty() {
        let mut spans: Vec<Span> = Vec::new();
        for (i, kb) in props.keybinds.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", Style::default().fg(theme.dim)));
            }
            spans.push(Span::styled(
                kb.keys.clone(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                format!(" {}", kb.title),
                Style::default().fg(theme.dim),
            ));
        }
        spans.push(Span::styled("   esc", Style::default().fg(theme.dim)));
        spans.push(Span::styled(" close", Style::default().fg(theme.dim)));
        let inner_hints = hints_area.inner(ratatui::layout::Margin {
            horizontal: 2,
            vertical: 0,
        });
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.bg_secondary)),
            inner_hints,
        );
    }

    DialogSelectLayout {
        outer: area,
        filter: filter_area,
        list: list_area,
        hints: hints_area,
        option_y_map,
    }
}

pub fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
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

/// Convert a flat filtered-index list to the "Nth Option row" counterpart.
/// Given a click at y, returns the option index if it hits an option row.
pub fn option_at_y(layout: &DialogSelectLayout, y: u16) -> Option<usize> {
    layout
        .option_y_map
        .iter()
        .find(|(row_y, _)| *row_y == y)
        .map(|(_, idx)| *idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_score_prefix_beats_substring() {
        let prefix = fuzzy_score("ver", "verify", None).unwrap();
        let sub = fuzzy_score("ver", "loverly", None).unwrap();
        assert!(prefix > sub);
    }

    #[test]
    fn fuzzy_score_allows_category_fallback() {
        let ranked = fuzzy_score("sess", "next", Some("session")).unwrap();
        assert!(ranked > 0);
    }

    #[test]
    fn fuzzy_score_rejects_non_matches() {
        assert!(fuzzy_score("xyz", "verify", None).is_none());
    }

    #[test]
    fn filter_and_flatten_respects_current_filter() {
        let opts = vec![
            DialogOption::new("a", "apple").category("fruit"),
            DialogOption::new("b", "banana").category("fruit"),
            DialogOption::new("c", "carrot").category("veg"),
        ];
        let mut state = DialogSelectState::default();
        state.filter = "ap".to_string();
        let (rows, filtered) = filter_and_flatten(&opts, &state, true);
        assert_eq!(filtered.len(), 1);
        assert_eq!(opts[filtered[0]].value, "a");
        assert!(
            rows.iter()
                .any(|r| matches!(r, Row::Option(i) if *i == filtered[0]))
        );
    }
}
