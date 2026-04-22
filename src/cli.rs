use crate::providers::factory::Provider;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Clone, Parser)]
#[command(name = "charm")]
#[command(
    about = "Interactive coding agent for large codebase operations",
    override_usage = "charm [OPTIONS] [PROMPT]\n       charm [OPTIONS] <COMMAND> [ARGS]"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(value_name = "PROMPT")]
    pub prompt: Option<String>,

    #[arg(short, long, global = true)]
    pub workspace: Option<PathBuf>,

    #[arg(short, long, global = true, default_value = "moonshotai/kimi-k2.6")]
    pub model: String,

    #[arg(long, global = true, value_enum, default_value = "auto")]
    pub provider: ProviderArg,

    #[arg(long, default_value_t = false, conflicts_with_all = ["continue_last", "session"])]
    pub new: bool,

    #[arg(
        long = "continue",
        default_value_t = false,
        conflicts_with_all = ["new", "session"]
    )]
    pub continue_last: bool,

    #[arg(long, conflicts_with_all = ["new", "continue_last"])]
    pub session: Option<String>,
}

impl Cli {
    pub fn interactive_request(&self) -> InteractiveRequest {
        InteractiveRequest {
            prompt: self.prompt.clone(),
            new_session: self.new,
            continue_last: self.continue_last,
            session_id: self.session.clone(),
        }
    }

    pub fn should_start_interactive(&self) -> bool {
        self.command.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveRequest {
    pub prompt: Option<String>,
    pub new_session: bool,
    pub continue_last: bool,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, clap::ValueEnum, PartialEq, Eq)]
pub enum ProviderArg {
    #[value(name = "auto")]
    Auto,
    #[value(name = "openai")]
    OpenAi,
    #[value(name = "openai-codex")]
    OpenAiCodex,
    #[value(name = "anthropic")]
    Anthropic,
    #[value(name = "google")]
    Google,
    #[value(name = "ollama")]
    Ollama,
    #[value(name = "openrouter")]
    OpenRouter,
}

impl ProviderArg {
    pub fn to_provider(&self) -> Option<Provider> {
        match self {
            ProviderArg::Auto => None,
            ProviderArg::OpenAi => Some(Provider::OpenAi),
            ProviderArg::OpenAiCodex => Some(Provider::OpenAiCodex),
            ProviderArg::Anthropic => Some(Provider::Anthropic),
            ProviderArg::Google => Some(Provider::Google),
            ProviderArg::Ollama => Some(Provider::Ollama),
            ProviderArg::OpenRouter => Some(Provider::OpenRouter),
        }
    }
}

#[derive(Debug, Clone, Subcommand, PartialEq)]
pub enum Command {
    Init,
    Index,
    Tools,
    Exec {
        tool: String,
        #[arg(short, long, default_value = "{}")]
        args: String,
    },
    Delegate {
        task: String,
        #[arg(long, default_value = "moonshotai/kimi-k2.6")]
        planner_model: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_parse_as_interactive_root() {
        let cli = Cli::try_parse_from(["charm"]).expect("parse");
        assert!(cli.should_start_interactive());
        assert_eq!(
            cli.interactive_request(),
            InteractiveRequest {
                prompt: None,
                new_session: false,
                continue_last: false,
                session_id: None,
            }
        );
    }

    #[test]
    fn prompt_parse_as_interactive_root() {
        let cli = Cli::try_parse_from(["charm", "explain the project"]).expect("parse");
        assert!(cli.should_start_interactive());
        assert_eq!(
            cli.interactive_request(),
            InteractiveRequest {
                prompt: Some("explain the project".to_string()),
                new_session: false,
                continue_last: false,
                session_id: None,
            }
        );
    }

    #[test]
    fn continue_flag_is_mutually_exclusive_with_new() {
        let err = Cli::try_parse_from(["charm", "--continue", "--new"]).expect_err("must fail");
        assert!(err.to_string().contains("--continue"));
    }

    #[test]
    fn legacy_run_command_is_removed_from_public_cli() {
        let err = Cli::try_parse_from(["charm", "run", "fix this"]).expect_err("must fail");
        let rendered = err.to_string();
        assert!(
            rendered.contains("unexpected argument")
                || rendered.contains("unrecognized subcommand")
                || rendered.contains("invalid value")
        );
    }

    #[test]
    fn parses_new_provider_flags() {
        let cli = Cli::try_parse_from(["charm", "--provider", "openai-codex"]).expect("parse");
        assert!(matches!(cli.provider, ProviderArg::OpenAiCodex));

        let cli = Cli::try_parse_from(["charm", "--provider", "google"]).expect("parse");
        assert!(matches!(cli.provider, ProviderArg::Google));
    }
}
