use ratatui::style::Color;

pub struct Theme {
    pub role_user: Color,
    pub role_assistant: Color,
    pub role_system: Color,
    pub role_tool: Color,
    pub role_router: Color,
    pub role_approval: Color,

    pub accent: Color,
    pub dim: Color,
    pub success: Color,
    pub error: Color,
    pub warning: Color,

    pub border: Color,
    pub border_focused: Color,

    pub bg_primary: Color,
    pub bg_secondary: Color,
    pub bg_highlight: Color,
    pub bg_composer: Color,

    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_muted: Color,

    pub spinner_frames: &'static [&'static str],
    pub spinner_interval_ms: u64,

    pub palette_selected_fg: Color,
    pub palette_selected_bg: Color,

    pub dock_title: Color,

    pub status_label: Color,
    pub status_value: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::charm_dark()
    }
}

impl Theme {
    pub fn charm_dark() -> Self {
        Self {
            role_user: Color::Rgb(80, 250, 123),
            role_assistant: Color::Rgb(139, 233, 253),
            role_system: Color::Rgb(189, 147, 249),
            role_tool: Color::Rgb(98, 174, 234),
            role_router: Color::Rgb(241, 250, 140),
            role_approval: Color::Rgb(255, 121, 198),

            accent: Color::Rgb(139, 233, 253),
            dim: Color::Rgb(88, 88, 108),
            success: Color::Rgb(80, 250, 123),
            error: Color::Rgb(255, 85, 85),
            warning: Color::Rgb(241, 250, 140),

            border: Color::Rgb(68, 71, 90),
            border_focused: Color::Rgb(139, 233, 253),

            bg_primary: Color::Reset,
            bg_secondary: Color::Rgb(30, 30, 46),
            bg_highlight: Color::Rgb(50, 50, 78),
            bg_composer: Color::Rgb(24, 24, 37),

            text_primary: Color::Reset,
            text_secondary: Color::Rgb(166, 173, 200),
            text_muted: Color::Rgb(88, 88, 108),

            spinner_frames: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
            spinner_interval_ms: 80,

            palette_selected_fg: Color::Rgb(30, 30, 46),
            palette_selected_bg: Color::Rgb(139, 233, 253),

            dock_title: Color::Rgb(139, 233, 253),

            status_label: Color::Rgb(241, 250, 140),
            status_value: Color::Rgb(205, 214, 244),
        }
    }

    pub fn charm_16() -> Self {
        Self {
            role_user: Color::Green,
            role_assistant: Color::Cyan,
            role_system: Color::Magenta,
            role_tool: Color::Blue,
            role_router: Color::Yellow,
            role_approval: Color::Red,

            accent: Color::Cyan,
            dim: Color::DarkGray,
            success: Color::Green,
            error: Color::Red,
            warning: Color::Yellow,

            border: Color::DarkGray,
            border_focused: Color::Cyan,

            bg_primary: Color::Reset,
            bg_secondary: Color::Reset,
            bg_highlight: Color::Reset,
            bg_composer: Color::Reset,

            text_primary: Color::Reset,
            text_secondary: Color::Reset,
            text_muted: Color::DarkGray,

            spinner_frames: &[".", "o", "O", "°", "O", "o"],
            spinner_interval_ms: 120,

            palette_selected_fg: Color::Black,
            palette_selected_bg: Color::Cyan,

            dock_title: Color::Cyan,

            status_label: Color::Yellow,
            status_value: Color::White,
        }
    }

    pub fn role_color(&self, role: &str) -> Color {
        match role {
            "user" => self.role_user,
            "assistant" => self.role_assistant,
            "system" => self.role_system,
            "tool" => self.role_tool,
            "router" => self.role_router,
            "approval" => self.role_approval,
            _ => self.text_secondary,
        }
    }
}
