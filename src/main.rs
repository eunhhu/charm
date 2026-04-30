use charm::agent::runner::AgentRunner;
use charm::cli::{Cli, Command, DEFAULT_MODEL, InteractiveLaunch};
use charm::orchestrator::{broker::Broker, roles::PlannerRole};
use charm::providers::factory::{
    Provider, ResolvedProviderSession, resolve_model_selection, resolve_provider_session,
};
use charm::providers::sse::{StreamChoice, StreamChunk, StreamDelta};
use charm::providers::types::{ChatRequest, Message, ToolSchema, Usage, default_tool_schemas};
use charm::runtime::session_runtime::{RuntimeModel, SessionRuntime};
use charm::runtime::types::RuntimeEvent;
use charm::tools::ToolRegistry;
use clap::Parser;
use std::path::Path;
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let (workspace, cli) = match Cli::parse().into_workspace_scope() {
        Ok(scoped) => scoped,
        Err(err) => err.exit(),
    };
    let cwd = workspace.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    if let Some(launch) = cli.interactive_launch() {
        return run_interactive_session(launch, &cwd, runtime);
    }

    match cli.command.clone().expect("checked above") {
        Command::Ask { .. }
        | Command::New { .. }
        | Command::Resume { .. }
        | Command::Session { .. }
        | Command::Model { .. }
        | Command::Workspace { .. } => unreachable!("interactive/scoped commands handled above"),
        Command::Init => {
            let charm_dir = cwd.join(".charm");
            std::fs::create_dir_all(&charm_dir)?;
            println!("Initialized Charm workspace at {}", charm_dir.display());
        }
        Command::Index => {
            let mut index = charm::indexer::types::Index::default();
            println!("Indexing workspace: {}", cwd.display());
            charm::indexer::parser::Indexer::index_workspace(&cwd, &mut index)?;
            let store = charm::indexer::store::IndexStore::new(&cwd);
            store.save(&index)?;
            println!(
                "Indexed {} files, {} symbols",
                index.files.len(),
                index.symbols.len()
            );
        }
        Command::Tools => {
            let registry = ToolRegistry::new(&cwd);
            println!("Available tools:");
            for name in registry.list_tools() {
                println!("  - {}", name);
            }
        }
        Command::Exec { tool, args } => {
            let mut registry = ToolRegistry::new(&cwd);
            let parsed_args: serde_json::Value = serde_json::from_str(&args)?;
            let result = runtime.block_on(registry.execute(&tool, parsed_args))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Command::Delegate { task } => {
            let task = task.join(" ");
            let planner_session = resolve_provider_session(None, DEFAULT_MODEL)?;
            println!("=== Planning Phase ===");
            let planner = PlannerRole::new(planner_session.client, planner_session.request_model);
            let context = format!("Workspace: {}", cwd.display());
            let decomposition = runtime.block_on(planner.decompose(&task, &context))?;

            println!("Decomposed into {} subtasks:", decomposition.subtasks.len());
            for subtask in &decomposition.subtasks {
                println!(
                    "  [{}] {} (deps: {:?})",
                    subtask.id, subtask.description, subtask.dependencies
                );
            }

            println!("\n=== Execution Phase ===");
            let execution_session = resolve_provider_session(None, DEFAULT_MODEL)?;
            let mut runner = AgentRunner::new(
                execution_session.client,
                &cwd,
                execution_session.request_model,
                execution_session.display_model,
                charm::agent::prompt::AgentMode::Build,
            )?;
            let broker = Broker::new();
            let results = runtime.block_on(broker.execute_plan(decomposition, &mut runner))?;

            println!("\n=== Results ===");
            for result in &results {
                println!(
                    "  [{}] success={} files={:?}",
                    result.subtask_id, result.success, result.files_changed
                );
            }
        }
    }

    Ok(())
}

fn run_interactive_session(
    launch: InteractiveLaunch,
    cwd: &Path,
    runtime: tokio::runtime::Runtime,
) -> anyhow::Result<()> {
    let prepared = prepare_interactive_provider(None, &launch.model)?;
    let display_model = prepared.display_model.clone();
    let request_model = prepared.request_model.clone();
    let client = prepared.client;
    let request = launch.request;
    let auto_prompt = request.prompt.clone();

    let (mut session_runtime, mut events) = runtime.block_on(SessionRuntime::bootstrap(
        cwd,
        request_model,
        display_model,
        request,
        client,
    ))?;
    events.extend(prepared.initial_events);

    if let Some(prompt) = auto_prompt
        && !prompt.trim().is_empty()
    {
        let prompt_events = runtime.block_on(session_runtime.submit_input(&prompt))?;
        events.extend(prompt_events);
    }

    charm::tui::app::run_session_tui(session_runtime, runtime, events)
}

struct InteractiveProviderResolution {
    request_model: String,
    display_model: String,
    client: Arc<dyn RuntimeModel>,
    initial_events: Vec<RuntimeEvent>,
}

fn prepare_interactive_provider(
    preferred: Option<Provider>,
    raw_model: &str,
) -> anyhow::Result<InteractiveProviderResolution> {
    prepare_interactive_provider_inner(preferred, raw_model, resolve_provider_session)
}

fn prepare_interactive_provider_inner<F>(
    preferred: Option<Provider>,
    raw_model: &str,
    resolver: F,
) -> anyhow::Result<InteractiveProviderResolution>
where
    F: FnOnce(Option<Provider>, &str) -> anyhow::Result<ResolvedProviderSession>,
{
    let selection = resolve_model_selection(preferred, raw_model)?;
    match resolver(preferred, raw_model) {
        Ok(resolved) => Ok(InteractiveProviderResolution {
            request_model: resolved.request_model,
            display_model: resolved.display_model,
            client: Arc::new(resolved.client),
            initial_events: Vec::new(),
        }),
        Err(err) => {
            let provider_id = selection.provider.id();
            let content =
                provider_connection_required_content(provider_id, &selection.display_model, &err);
            Ok(InteractiveProviderResolution {
                request_model: selection.request_model,
                display_model: selection.display_model,
                client: Arc::new(UnavailableModel::new(content.clone())),
                initial_events: vec![RuntimeEvent::Modal {
                    title: "Provider Connection Required".to_string(),
                    content,
                }],
            })
        }
    }
}

fn provider_connection_required_content(
    provider_id: &str,
    display_model: &str,
    err: &anyhow::Error,
) -> String {
    format!(
        "Model `{display_model}` needs provider `{provider_id}`.\n\n\
         Charm can connect it inside this REPL:\n  /provider connect {provider_id}\n\n\
         Secrets are saved locally at ~/.charm/auth.json.\n\n\
         Resolver detail: {err}"
    )
}

struct UnavailableModel {
    message: String,
}

impl UnavailableModel {
    fn new(message: String) -> Self {
        Self { message }
    }

    fn assistant_message(&self) -> Message {
        Message {
            role: "assistant".to_string(),
            content: Some(self.message.clone()),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            reasoning_details: None,
        }
    }
}

#[async_trait::async_trait]
impl RuntimeModel for UnavailableModel {
    async fn chat(&self, _request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)> {
        Ok((self.assistant_message(), None))
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamChunk>>> {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let _ = tx
            .send(Ok(StreamChunk {
                id: Some("provider-unavailable".to_string()),
                object: None,
                created: None,
                model: Some(request.model),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: Some("assistant".to_string()),
                        content: Some(self.message.clone()),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: None,
            }))
            .await;
        Ok(rx)
    }

    fn tool_schemas(&self) -> Vec<ToolSchema> {
        default_tool_schemas()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use charm::providers::factory::Provider;
    use charm::runtime::types::RuntimeEvent;

    #[test]
    fn interactive_provider_resolution_falls_back_to_repl_auth_modal_when_auth_missing() {
        let prepared = prepare_interactive_provider_inner(
            Some(Provider::OpenRouter),
            "openrouter/moonshotai/kimi-k2.6",
            |_preferred, _model| anyhow::bail!("OPENROUTER_API_KEY must be set"),
        )
        .expect("prepare");

        assert_eq!(prepared.request_model, "moonshotai/kimi-k2.6");
        assert_eq!(prepared.display_model, "openrouter/moonshotai/kimi-k2.6");
        assert!(prepared.initial_events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Modal { title, content }
                if title.contains("Provider")
                    && content.contains("/provider connect openrouter")
                    && content.contains("~/.charm/auth.json")
        )));
    }
}
