use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::executions::TrackedAgent;

use crate::{
    cli::{
        args::internal::InternalCommand,
        commands::{
            agent_state, approvals, environment, events, menu, messages, progress, sessions,
            subagents, worktree_hooks,
        },
        context::Context,
        terminal,
    },
    sessions::HostName,
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn run(ctx: Context, command: InternalCommand) -> Result<i32> {
    match command {
        InternalCommand::Events { follow, after } => events::run(&ctx, follow, after).await,
        InternalCommand::Env { space, revoke } => environment::run(&ctx, space, revoke).await,
        InternalCommand::MenuRows { view } => menu::print_rows(&ctx, view).await,
        InternalCommand::MenuNext => menu::print_next(&ctx),
        InternalCommand::ApprovalRows { space } => approvals::print_rows(&ctx, space).await,
        InternalCommand::ApprovalDecide { decision, row } => {
            approvals::decide_row(&ctx, &row, decision).await
        }
        InternalCommand::ProxyReport { report } => {
            shoal_workers::proxy::send_relayed(&ctx.paths, &report).await
        }
        InternalCommand::AgentState {
            state,
            ends_turn,
            space,
        } => agent_state::run(&ctx, space, state, ends_turn).await,
        InternalCommand::Plan { integration, space } => {
            progress::report_plan(&ctx, space, integration).await
        }
        InternalCommand::TurnFailed { integration, space } => {
            agent_state::turn_failed(&ctx, space, integration).await
        }
        InternalCommand::Wake {
            agent,
            space,
            handed_over,
            resume,
        } => messages::wake(&ctx, agent, space.or(handed_over), resume).await,
        InternalCommand::SubagentStart => subagents::start(&ctx).await,
        InternalCommand::Subagent { integration, space } => {
            subagents::report(&ctx, space, integration).await
        }
        InternalCommand::WorktreeCreate => worktree_hooks::create(&ctx).await,
        InternalCommand::WorktreeRemove => worktree_hooks::remove(&ctx).await,
        InternalCommand::PlaceWatch {
            host,
            workspace,
            place,
            instance,
            kept_at,
        } => {
            let place = crate::terminal::Watched {
                id: place,
                workspace,
                instance,
            };
            terminal::watch_place(&ctx, host, place, kept_at).await
        }
        InternalCommand::HerdrWatch { workspace, tab } => {
            let place = crate::terminal::Watched {
                id: tab,
                workspace,
                instance: None,
            };
            terminal::watch_place(&ctx, HostName::Herdr, place, None).await
        }
        InternalCommand::Merge { result, plan } => {
            crate::operations::merge_worker(&ctx.paths, plan, result).await
        }
        InternalCommand::Unmerge { result, plan } => {
            crate::operations::unmerge_worker(&ctx.paths, plan, result).await
        }
        InternalCommand::CopyIgnored { exclude, setup_cmd } => {
            crate::execution::copy_ignored_worker(&exclude, setup_cmd).await
        }
        InternalCommand::Detached {
            workspace,
            log,
            agent,
            model,
            mode,
            narrowing,
            new_session,
            command,
        } => {
            let agent = agent.map(|name| TrackedAgent {
                name,
                model: model.into(),
                mode,
                narrowing,
                new_session,
            });
            shoal_workers::execution::run_detached_wrapper(
                &ctx.paths,
                SpaceSelector::Id(workspace),
                log,
                command,
                agent,
            )
            .await
        }
        InternalCommand::SecretTool {
            launchers,
            tool,
            args,
        } => shoal_workers::secrets::run_tool(&ctx.paths, &launchers, &tool, args).await,
        InternalCommand::PushGate { remote, url } => {
            shoal_workers::push_gate::serve(&ctx.paths, remote, url).await
        }
        InternalCommand::GitCredential { operation } => {
            shoal_workers::git_credential::answer(&ctx.paths, operation).await
        }
        InternalCommand::QueueStart { entry } => crate::queued_starts::run(&ctx.paths, entry).await,
        InternalCommand::Operation => crate::operations::run(&ctx.paths).await,
        InternalCommand::Launch { file } => crate::terminal::run_launch(&file),
        InternalCommand::TakeOver {
            workspace,
            agent,
            model,
            mode,
            narrowing,
            connection,
            record,
            log,
        } => {
            let taken = shoal_workers::execution::TakenOver {
                workspace,
                // The wrapper it continues registered the agent's start.
                agent: TrackedAgent {
                    name: agent,
                    model: model.into(),
                    mode,
                    narrowing,
                    new_session: false,
                },
                connection,
                record,
                log,
            };
            shoal_workers::execution::take_over(&ctx.paths, taken).await
        }
        InternalCommand::Place(args) => terminal::place_worker(ctx, args).await,
        InternalCommand::Demo {
            workspace,
            launch,
            command,
        } => {
            let launch = serde_json::from_str(&launch)
                .map_err(|error| anyhow::anyhow!("invalid demo launch: {error}"))?;
            shoal_workers::execution::run_demo(
                &ctx.paths,
                SpaceSelector::Id(workspace),
                launch,
                command,
            )
            .await
        }
        InternalCommand::Attach {
            space,
            viewer,
            machine,
            report,
        } => sessions::attach_for_viewer(&ctx, space, viewer, machine, report).await,
        InternalCommand::Session {
            space,
            agent,
            model,
            mode,
            narrowing,
            new_session,
            records,
            command,
            host,
            close_in,
            manual,
        } => {
            // `host` opened this place for the agent.
            terminal::record_owned_here(&ctx, host, &space).await;
            if let Some(host) = close_in {
                terminal::close_here_when_done(&ctx, host, &space).await;
            }
            let agent = TrackedAgent {
                name: agent,
                model: model.into(),
                mode,
                narrowing,
                new_session,
            };
            let source = manual.then_some(shoal_protocol::launches::LaunchSource::Manual);
            run_session(&ctx, space, agent, (records, source), command).await
        }
    }
}

/// Run a session's tracked agent, consuming `records` once it starts, a
/// restore made by `source` when it says.
async fn run_session(
    ctx: &Context,
    space: SpaceId,
    agent: TrackedAgent,
    (records, source): (
        Vec<std::path::PathBuf>,
        Option<shoal_protocol::launches::LaunchSource>,
    ),
    command: Vec<std::ffi::OsString>,
) -> Result<i32> {
    if records.is_empty() {
        return shoal_workers::execution::run(
            &ctx.paths,
            SpaceSelector::Id(space),
            command,
            Some(agent),
        )
        .await;
    }
    shoal_workers::execution::run_recovery(
        &ctx.paths,
        SpaceSelector::Id(space),
        command,
        agent,
        (records, source),
    )
    .await
}
