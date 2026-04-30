pub mod app;
pub mod dialog;
pub mod event;
pub mod theme;

use colored::*;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

pub struct Spinner {
    bar: ProgressBar,
}

impl Spinner {
    pub fn new(msg: &str) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap(),
        );
        bar.set_message(msg.to_string());
        bar.enable_steady_tick(Duration::from_millis(100));
        Self { bar }
    }

    pub fn finish(&self, msg: &str) {
        self.bar.finish_with_message(msg.green().to_string());
    }

    pub fn finish_error(&self, msg: &str) {
        self.bar.finish_with_message(msg.red().to_string());
    }
}

pub fn header(text: &str) -> String {
    format!("\n{}", text.bold().underline())
}

pub fn section(title: &str, content: &str) -> String {
    format!("{}\n{}", title.bold().blue(), content)
}

pub fn token_display(input: u32, output: u32, reasoning: u32) -> String {
    let mut parts = vec![
        format!("{} {} in", "◉".blue(), input),
        format!("{} {} out", "◉".green(), output),
    ];
    if reasoning > 0 {
        parts.push(format!("{} {} reasoning", "◉".yellow(), reasoning));
    }
    format!("[{}]", parts.join(" / "))
}

pub fn tool_call(name: &str, args: &str) -> String {
    format!("  → {} {}", name.cyan(), args.dimmed())
}

pub fn tool_success(line: &str) -> String {
    format!("    {} {}", "✓".green(), line)
}

pub fn tool_error(line: &str) -> String {
    format!("    {} {}", "✗".red(), line)
}

pub fn turn_header(turn: usize, max: usize, budget: usize) -> String {
    format!(
        "\n{} {}",
        format!("[Turn {}/{}]", turn, max).bold(),
        format!("budget: {}", budget).dimmed()
    )
}

pub fn agent_thought(line: &str) -> String {
    format!("  {} {}", "Agent:".italic().dimmed(), line)
}

pub fn prism_hint(files: &[String]) -> String {
    format!(
        "  {} {}",
        "[Prism]".purple(),
        format!("Related: {}", files.join(", ")).dimmed()
    )
}

pub fn savings_report(original: usize, saved: u32) -> String {
    format!(
        "  {} {} → {} ({}% saved)",
        "[RTK]".yellow(),
        original,
        original - saved as usize,
        saved
    )
}

pub fn status_badge(status: &str) -> String {
    match status {
        "passed" => " ● PASSED ".on_green().black().to_string(),
        "failed" => " ● FAILED ".on_red().white().to_string(),
        "error" => " ● ERROR ".on_yellow().black().to_string(),
        _ => format!(" ● {} ", status.to_uppercase())
            .on_blue()
            .white()
            .to_string(),
    }
}
