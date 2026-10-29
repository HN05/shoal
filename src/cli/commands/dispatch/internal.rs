use anyhow::Result;

use crate::cli::{
    args::internal::InternalCommand,
    commands::{
        agent_queue, agent_state, approvals, events, menu, messages, subagents, workspaces,
        worktree_hooks,
    },
    context::Context,
    terminal,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: InternalCommand) -> Result<i32> {
    match command {
        InternalCommand::Events { follow, since } => events::run(&ctx, follow, since).await,
        InternalCommand::MenuRows { view } => menu::print_rows(&ctx, view).await,
        InternalCommand::MenuNext => menu::print_next(&ctx),
        InternalCommand::ApprovalRows { workspace } => approvals::print_rows(&ctx, workspace).await,
        InternalCommand::ApprovalDecide { decision, row } => {
            approvals::decide_row(&ctx, &row, decision).await
        }
        InternalCommand::ProxyReport { report } => {
            crate::proxy::send_relayed(&ctx.paths, &report).await
        }
        InternalCommand::AgentState {
            state,
            ends_turn,
            workspace,
        } => agent_state::run(&ctx, workspace, state, ends_turn).await,
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
            instance,
            kept_at,
        } => {
            let place = terminal::Watched {
                id: place,
                workspace,
                instance,
            };
            terminal::watch_place(&ctx, host, place, kept_at).await
        }
        InternalCommand::HerdrWatch { workspace, tab } => {
            let place = terminal::Watched {
                id: tab,
                workspace,
                instance: None,
            };
            terminal::watch_place(&ctx, "herdr".into(), place, None).await
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
        InternalCommand::Launch { file } => terminal::run_launch(&file),
        InternalCommand::TakeOver {
            workspace,
            agent,
            connection,
            record,
            log,
        } => {
            let taken = crate::execution::TakenOver {
                workspace,
                agent,
                connection,
                record,
                log,
            };
            crate::execution::take_over(&ctx.paths, taken).await
        }
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
            host,
            close_in,
        } => {
            // `host` opened this place for the agent.
            terminal::record_owned_here(&ctx, &host, &workspace).await;
            if let Some(host) = close_in {
                terminal::close_here_when_done(&ctx, &host, &workspace).await;
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
