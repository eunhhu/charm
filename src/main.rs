use charm::agent::runner::AgentRunner;
use charm::cli::{Cli, Command};
use charm::orchestrator::{broker::Broker, roles::PlannerRole};
use charm::providers::factory::resolve_provider_session;
use charm::runtime::session_runtime::{RuntimeModel, SessionRuntime};
use charm::tools::ToolRegistry;
use clap::Parser;
use std::path::Path;
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    let cwd = cli
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    if cli.should_start_interactive() {
        return run_interactive_session(&cli, &cwd, &runtime);
    }

    match cli.command.clone().expect("checked above") {
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
        Command::Delegate {
            task,
            planner_model,
        } => {
            let planner_session =
                resolve_provider_session(cli.provider.to_provider(), &planner_model)?;
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
            let execution_session =
                resolve_provider_session(cli.provider.to_provider(), &cli.model)?;
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
    cli: &Cli,
    cwd: &Path,
    runtime: &tokio::runtime::Runtime,
) -> anyhow::Result<()> {
    let resolved = resolve_provider_session(cli.provider.to_provider(), &cli.model)?;
    let display_model = resolved.display_model.clone();
    let request_model = resolved.request_model.clone();
    let client: Arc<dyn RuntimeModel> = Arc::new(resolved.client);
    let request = cli.interactive_request();
    let auto_prompt = request.prompt.clone();

    let (mut session_runtime, mut events) = runtime.block_on(SessionRuntime::bootstrap(
        cwd,
        request_model,
        display_model,
        request,
        client,
    ))?;

    if let Some(prompt) = auto_prompt {
        if !prompt.trim().is_empty() {
            let prompt_events = runtime.block_on(session_runtime.submit_input(&prompt))?;
            events.extend(prompt_events);
        }
    }

    charm::tui::app::run_session_tui(&mut session_runtime, runtime, events)
}
