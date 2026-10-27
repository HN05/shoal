use anyhow::Result;

use crate::cli::{
    args::internal::InternalCommand,
    commands::{
        agent_queue, agent_state, events, menu, messages, subagents, workspaces, worktree_hooks,
    },
    context::Context,
    terminal,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: InternalCommand) -> Result<i32> {
    match command {
        InternalCommand::Events { follow, since } => events::run(&ctx, follow, since).await,
        InternalCommand::MenuRows { view } => menu::print_rows(&ctx, view).await,
        InternalCommand::MenuNext => menu::print_next(&ctx),
        InternalCommand::AgentState { state, workspace } => {
            agent_state::run(&ctx, workspace, state).await
        }
        InternalCommand::TurnFailed {
            integration,
            workspace,
        } => agent_state::turn_failed(&ctx, workspace, integration).await,
        InternalCommand::Wake {
            agent,
            workspace,
            resume,
        } => messages::wake(&ctx, agent, workspace, resume).await,
        InternalCommand::SubagentStart => subagents::start(&ctx).await,
        InternalCommand::WorktreeCreate => worktree_hooks::create(&ctx).await,
        InternalCommand::WorktreeRemove => worktree_hooks::remove(&ctx).await,
        InternalCommand::PlaceWatch {
            host,
            workspace,
            place,
        } => terminal::watch_place(&ctx, host, workspace, place).await,
        InternalCommand::HerdrWatch { workspace, tab } => {
            terminal::watch_place(&ctx, "herdr".into(), workspace, tab).await
        }
        InternalCommand::Land { push, plan } => workspaces::land_worker(&ctx, plan, push).await,
        InternalCommand::CopyIgnored { setup_cmd } => {
            workspaces::copy_ignored_worker(&ctx, setup_cmd).await
        }
        InternalCommand::Detached {
            workspace,
            log,
            agent,
            command,
        } => {
            crate::execution::run_detached_wrapper(&ctx.paths, workspace, log, command, agent).await
        }
        InternalCommand::SecretTool {
            launchers,
            tool,
            args,
        } => crate::secrets::run_tool(&ctx.paths, &launchers, &tool, args).await,
        InternalCommand::QueueStart { entry } => agent_queue::start(&ctx, entry).await,
        InternalCommand::Add {
            host,
            close_when_done,
            plan,
        } => terminal::add_worker(ctx, &host, close_when_done, &plan).await,
        InternalCommand::Demo {
            workspace,
            launch,
            command,
        } => {
            let launch = serde_json::from_str(&launch)
                .map_err(|error| anyhow::anyhow!("invalid demo launch: {error}"))?;
            crate::execution::run_demo(&ctx.paths, workspace, launch, command).await
        }
        InternalCommand::Session {
            workspace,
            agent,
            records,
            command,
            close_in,
        } => {
            if let Some(host) = close_in {
                terminal::close_here_when_done(&ctx, &host, &workspace);
            }
            run_session(&ctx, workspace, agent, records, command).await
        }
    }
}

/// Run a session's tracked agent, consuming `records` once it starts.
async fn run_session(
    ctx: &Context,
    workspace: String,
    agent: String,
    records: Vec<std::path::PathBuf>,
    command: Vec<std::ffi::OsString>,
) -> Result<i32> {
    if records.is_empty() {
        return crate::execution::run(&ctx.paths, workspace, command, Some(agent)).await;
    }
    crate::execution::run_recovery(&ctx.paths, workspace, command, agent, records).await
}
