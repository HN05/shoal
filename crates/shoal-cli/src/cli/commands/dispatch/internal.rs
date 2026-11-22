use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::executions::TrackedAgent;
use shoal_workers::agent_hooks;
use shoal_workers::hosting;

use crate::{
    cli::{
        args::internal::InternalCommand,
        commands::{approvals, demo, environment, events, menu, sessions},
        context::Context,
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
        } => {
            let report = (state, ends_turn);
            agent_hooks::turns::report_state(&ctx.paths, ctx.json, space, report).await
        }
        InternalCommand::Change { integration } => {
            agent_hooks::changes::report(&ctx.paths, integration).await
        }
        InternalCommand::Plan { integration, space } => {
            agent_hooks::plan::report(&ctx.paths, space, integration).await
        }
        InternalCommand::TurnFailed { integration, space } => {
            agent_hooks::turns::report_failure(&ctx.paths, ctx.json, space, integration).await
        }
        InternalCommand::Wake {
            agent,
            space,
            handed_over,
            resume,
        } => agent_hooks::delivery::wake(&ctx.paths, agent, space.or(handed_over), resume).await,
        InternalCommand::Deliver => agent_hooks::delivery::deliver(&ctx.paths).await,
        InternalCommand::SubagentStart => agent_hooks::subagents::start(&ctx.paths).await,
        InternalCommand::Subagent { integration, space } => {
            agent_hooks::subagents::report(&ctx.paths, space, integration).await
        }
        InternalCommand::WorktreeCreate => {
            agent_hooks::worktrees::create(&ctx.paths, ctx.json).await
        }
        InternalCommand::WorktreeRemove => {
            agent_hooks::worktrees::remove(&ctx.paths, ctx.json).await
        }
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
            shoal_workers::watchers::watch(&ctx.paths, host, place, kept_at).await
        }
        InternalCommand::HerdrWatch { workspace, tab } => {
            let place = crate::terminal::Watched {
                id: tab,
                workspace,
                instance: None,
            };
            shoal_workers::watchers::watch(&ctx.paths, HostName::Herdr, place, None).await
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
        InternalCommand::ImportMoved { ticket, no_resume } => {
            super::super::restore::import::import_moved(&ctx, ticket, !no_resume).await
        }
        InternalCommand::PlaceSessions { repository } => {
            crate::cli::agents::place_forked_sessions(&ctx, repository).await
        }
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
        InternalCommand::Place(args) => crate::cli::commands::place_worker(ctx, args).await,
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
        InternalCommand::ShowDemo {
            name,
            place,
            machine,
            app,
        } => demo::show_for_viewer(&name, &place, &machine, app.as_deref()).await,
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
            hosting::record_owned_here(ctx.caller(), host, &space).await;
            if let Some(host) = close_in {
                hosting::close_here_when_done(ctx.caller(), host, &space).await;
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
