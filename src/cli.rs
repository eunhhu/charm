use clap::{Parser, Subcommand};
use std::path::PathBuf;

pub const DEFAULT_MODEL: &str = "moonshotai/kimi-k2.6";

#[derive(Debug, Clone, Parser)]
#[command(name = "charm")]
#[command(
    about = "Interactive coding agent for large codebase operations",
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true,
    override_usage = "charm [PROMPT]\n       charm <COMMAND> [ARGS]"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(value_name = "PROMPT", num_args = 1.., trailing_var_arg = true)]
    pub prompt: Vec<String>,
}

impl Cli {
    pub fn interactive_launch(&self) -> Option<InteractiveLaunch> {
        match &self.command {
            None => Some(InteractiveLaunch::default().with_prompt(join_words(&self.prompt))),
            Some(Command::Ask { prompt }) => {
                Some(InteractiveLaunch::default().with_prompt(join_words(prompt)))
            }
            Some(Command::New { prompt }) => Some(
                InteractiveLaunch::default()
                    .with_new_session()
                    .with_prompt(join_words(prompt)),
            ),
            Some(Command::Resume { prompt }) => Some(
                InteractiveLaunch::default()
                    .with_continue_last()
                    .with_prompt(join_words(prompt)),
            ),
            Some(Command::Session { id, prompt }) => Some(
                InteractiveLaunch::default()
                    .with_session(id.clone())
                    .with_prompt(join_words(prompt)),
            ),
            Some(Command::Model { model, prompt }) => Some(
                InteractiveLaunch::default()
                    .with_model(model.clone())
                    .with_prompt(join_words(prompt)),
            ),
            Some(Command::Workspace { args, .. }) if args.is_empty() => {
                Some(InteractiveLaunch::default())
            }
            Some(Command::Workspace { .. }) => None,
            Some(Command::Init | Command::Index | Command::Tools | Command::Exec { .. }) => None,
            Some(Command::Delegate { .. }) => None,
        }
    }

    pub fn into_workspace_scope(mut self) -> Result<(Option<PathBuf>, Self), clap::Error> {
        let mut workspace = None;
        loop {
            match self.command {
                Some(Command::Workspace { path, args }) => {
                    workspace = Some(path);
                    self = if args.is_empty() {
                        Self {
                            command: None,
                            prompt: Vec::new(),
                        }
                    } else {
                        let argv = std::iter::once("charm".to_string()).chain(args);
                        Self::try_parse_from(argv)?
                    };
                }
                _ => return Ok((workspace, self)),
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InteractiveRequest {
    pub prompt: Option<String>,
    pub new_session: bool,
    pub continue_last: bool,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveLaunch {
    pub request: InteractiveRequest,
    pub model: String,
}

impl Default for InteractiveLaunch {
    fn default() -> Self {
        Self {
            request: InteractiveRequest::default(),
            model: DEFAULT_MODEL.to_string(),
        }
    }
}

impl InteractiveLaunch {
    fn with_prompt(mut self, prompt: Option<String>) -> Self {
        self.request.prompt = prompt;
        self
    }

    fn with_new_session(mut self) -> Self {
        self.request.new_session = true;
        self
    }

    fn with_continue_last(mut self) -> Self {
        self.request.continue_last = true;
        self
    }

    fn with_session(mut self, id: String) -> Self {
        self.request.session_id = Some(id);
        self
    }

    fn with_model(mut self, model: String) -> Self {
        self.model = model;
        self
    }
}

#[derive(Debug, Clone, Subcommand, PartialEq)]
pub enum Command {
    /// Start an interactive session with an optional prompt.
    Ask {
        #[arg(value_name = "PROMPT", num_args = 0.., trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// Start a fresh session.
    New {
        #[arg(value_name = "PROMPT", num_args = 0.., trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// Resume the most recent session.
    #[command(alias = "continue")]
    Resume {
        #[arg(value_name = "PROMPT", num_args = 0.., trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// Resume a session by id prefix.
    Session {
        id: String,
        #[arg(value_name = "PROMPT", num_args = 0.., trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// Start with a model, using provider/model when needed.
    Model {
        model: String,
        #[arg(value_name = "PROMPT", num_args = 0.., trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// Start in another workspace.
    Workspace {
        path: PathBuf,
        #[arg(value_name = "COMMAND", num_args = 0.., trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Initialize .charm metadata in the current workspace.
    Init,
    /// Build the workspace symbol index.
    Index,
    /// List available tools.
    Tools,
    /// Run a tool once with positional JSON args.
    Exec {
        tool: String,
        #[arg(value_name = "ARGS", default_value = "{}")]
        args: String,
    },
    /// Decompose and execute a task with planner/executor roles.
    Delegate {
        #[arg(value_name = "TASK", num_args = 1.., trailing_var_arg = true)]
        task: Vec<String>,
    },
}

fn join_words(words: &[String]) -> Option<String> {
    let joined = words.join(" ");
    if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_parse_as_default_interactive_root() {
        let cli = Cli::try_parse_from(["charm"]).expect("parse");
        let launch = cli.interactive_launch().expect("interactive");
        assert_eq!(launch.model, DEFAULT_MODEL);
        assert_eq!(launch.request, InteractiveRequest::default());
    }

    #[test]
    fn bare_prompt_still_starts_interactive() {
        let cli = Cli::try_parse_from(["charm", "explain", "the", "project"]).expect("parse");
        let launch = cli.interactive_launch().expect("interactive");
        assert_eq!(
            launch.request.prompt.as_deref(),
            Some("explain the project")
        );
    }

    #[test]
    fn session_controls_are_subcommands() {
        let cli = Cli::try_parse_from(["charm", "new", "fresh", "task"]).expect("parse");
        let launch = cli.interactive_launch().expect("interactive");
        assert!(launch.request.new_session);
        assert_eq!(launch.request.prompt.as_deref(), Some("fresh task"));

        let cli = Cli::try_parse_from(["charm", "resume"]).expect("parse");
        let launch = cli.interactive_launch().expect("interactive");
        assert!(launch.request.continue_last);

        let cli = Cli::try_parse_from(["charm", "session", "abc123"]).expect("parse");
        let launch = cli.interactive_launch().expect("interactive");
        assert_eq!(launch.request.session_id.as_deref(), Some("abc123"));
    }

    #[test]
    fn model_is_a_subcommand_not_a_global_flag() {
        let cli =
            Cli::try_parse_from(["charm", "model", "openai/gpt-4.1", "fix", "bug"]).expect("parse");
        let launch = cli.interactive_launch().expect("interactive");
        assert_eq!(launch.model, "openai/gpt-4.1");
        assert_eq!(launch.request.prompt.as_deref(), Some("fix bug"));

        assert!(Cli::try_parse_from(["charm", "--model", "openai/gpt-4.1"]).is_err());
        assert!(Cli::try_parse_from(["charm", "--provider", "openai"]).is_err());
        assert!(Cli::try_parse_from(["charm", "--continue"]).is_err());
    }

    #[test]
    fn workspace_is_a_subcommand() {
        let cli = Cli::try_parse_from(["charm", "workspace", "/tmp/charm-ws"]).expect("parse");
        let (workspace, scoped) = cli.into_workspace_scope().expect("scope");
        assert_eq!(workspace.unwrap(), PathBuf::from("/tmp/charm-ws"));
        assert!(scoped.interactive_launch().is_some());
        assert!(Cli::try_parse_from(["charm", "--workspace", "/tmp/charm-ws"]).is_err());
    }

    #[test]
    fn workspace_subcommand_scopes_nested_commands() {
        let cli = Cli::try_parse_from([
            "charm",
            "workspace",
            "/tmp/charm-ws",
            "model",
            "ollama/qwen",
        ])
        .expect("parse");
        let (workspace, scoped) = cli.into_workspace_scope().expect("scope");

        assert_eq!(workspace.unwrap(), PathBuf::from("/tmp/charm-ws"));
        let launch = scoped.interactive_launch().expect("interactive");
        assert_eq!(launch.model, "ollama/qwen");

        let cli =
            Cli::try_parse_from(["charm", "workspace", "/tmp/charm-ws", "index"]).expect("parse");
        let (workspace, scoped) = cli.into_workspace_scope().expect("scope");

        assert_eq!(workspace.unwrap(), PathBuf::from("/tmp/charm-ws"));
        assert_eq!(scoped.command, Some(Command::Index));
    }

    #[test]
    fn exec_and_delegate_use_positional_arguments() {
        let cli = Cli::try_parse_from([
            "charm",
            "exec",
            "read_range",
            r#"{"file_path":"Cargo.toml"}"#,
        ])
        .expect("parse");
        assert_eq!(
            cli.command,
            Some(Command::Exec {
                tool: "read_range".to_string(),
                args: r#"{"file_path":"Cargo.toml"}"#.to_string(),
            })
        );

        let cli =
            Cli::try_parse_from(["charm", "delegate", "split", "this", "task"]).expect("parse");
        assert_eq!(
            cli.command,
            Some(Command::Delegate {
                task: vec!["split".to_string(), "this".to_string(), "task".to_string()],
            })
        );
    }
}
