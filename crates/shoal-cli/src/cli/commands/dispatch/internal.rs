use anyhow::Result;
use shoal_protocol::executions::TrackedAgent;
use shoal_workers::agent_hooks;
use shoal_workers::hosting;

use crate::{
    cli::{
        args::internal::InternalCommand,
        commands::{approvals, environment, events, menu, sessions},
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
            shoal_workers::launch::place_forked_sessions(&ctx.paths, repository).await
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
        } => hosting::show_for_viewer(&name, &place, &machine, app.as_deref()).await,
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
            let agent = TrackedAgent {
                name: agent,
                model: model.into(),
                mode,
                narrowing,
                new_session,
            };
            let session = hosting::Session {
                space,
                agent,
                host,
                close_in,
                records,
                manual,
                command,
            };
            hosting::run_session(ctx.caller(), session).await
        }
    }
}
