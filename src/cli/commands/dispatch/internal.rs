use crate::client::space_ids::SpaceId;
use anyhow::Result;

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        args::internal::InternalCommand,
        commands::{
            agent_state, approvals, events, git_credential, menu, messages, operations, progress,
            push_gate, queued_agents, subagents, workspaces, worktree_hooks,
        },
        context::Context,
        terminal,
    },
    execution::TrackedAgent,
    sessions::HostName,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: InternalCommand) -> Result<i32> {
    match command {
        InternalCommand::Events { follow, after } => events::run(&ctx, follow, after).await,
        InternalCommand::MenuRows { view } => menu::print_rows(&ctx, view).await,
        InternalCommand::MenuNext => menu::print_next(&ctx),
        InternalCommand::ApprovalRows { space } => approvals::print_rows(&ctx, space).await,
        InternalCommand::ApprovalDecide { decision, row } => {
            approvals::decide_row(&ctx, &row, decision).await
        }
        InternalCommand::ProxyReport { report } => {
            crate::proxy::send_relayed(&ctx.paths, &report).await
        }
        InternalCommand::AgentState {
            state,
            ends_turn,
            space,
        } => {
            agent_state::run(
                &ctx,
                space.as_deref().map(SpaceSelector::parse),
                state,
                ends_turn,
            )
            .await
        }
        InternalCommand::Plan { integration, space } => {
            let space = space.as_deref().map(SpaceSelector::parse);
            progress::report_plan(&ctx, space, integration).await
        }
        InternalCommand::TurnFailed { integration, space } => {
            agent_state::turn_failed(
                &ctx,
                space.as_deref().map(SpaceSelector::parse),
                integration,
            )
            .await
        }
        InternalCommand::Wake {
            agent,
            space,
            handed_over,
            resume,
        } => {
            messages::wake(
                &ctx,
                agent,
                space.as_deref().map(SpaceSelector::parse).or(handed_over),
                resume,
            )
            .await
        }
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
            workspaces::merge_worker(&ctx, plan, result).await
        }
        InternalCommand::Unmerge { result, plan } => {
            workspaces::unmerge_worker(&ctx, plan, result).await
        }
        InternalCommand::CopyIgnored { setup_cmd } => {
            workspaces::copy_ignored_worker(&ctx, setup_cmd).await
        }
        InternalCommand::Detached {
            workspace,
            log,
            agent,
            model,
            mode,
            narrowing,
            command,
        } => {
            let agent = agent.map(|name| TrackedAgent {
                name,
                model: model.into(),
                mode,
                narrowing,
            });
            crate::execution::run_detached_wrapper(
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
        } => crate::secrets::run_tool(&ctx.paths, &launchers, &tool, args).await,
        InternalCommand::PushGate { remote, url } => push_gate::run(&ctx, remote, url).await,
        InternalCommand::GitCredential { operation } => git_credential::run(&ctx, operation).await,
        InternalCommand::QueueStart { entry } => queued_agents::start(&ctx, entry).await,
        InternalCommand::Operation => operations::worker(&ctx).await,
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
            let taken = crate::execution::TakenOver {
                workspace,
                agent: TrackedAgent {
                    name: agent,
                    model: model.into(),
                    mode,
                    narrowing,
                },
                connection,
                record,
                log,
            };
            crate::execution::take_over(&ctx.paths, taken).await
        }
        InternalCommand::Add {
            host,
            close_when_done,
            focused,
            plan,
        } => terminal::add_worker(ctx, host, close_when_done, focused, &plan).await,
        InternalCommand::Demo {
            workspace,
            launch,
            command,
        } => {
            let launch = serde_json::from_str(&launch)
                .map_err(|error| anyhow::anyhow!("invalid demo launch: {error}"))?;
            crate::execution::run_demo(&ctx.paths, SpaceSelector::Id(workspace), launch, command)
                .await
        }
        InternalCommand::Session {
            workspace,
            agent,
            model,
            mode,
            narrowing,
            records,
            command,
            host,
            close_in,
        } => {
            // `host` opened this place for the agent.
            terminal::record_owned_here(&ctx, host, &workspace).await;
            if let Some(host) = close_in {
                terminal::close_here_when_done(&ctx, host, &workspace).await;
            }
            let agent = TrackedAgent {
                name: agent,
                model: model.into(),
                mode,
                narrowing,
            };
            run_session(&ctx, workspace, agent, records, command).await
        }
    }
}

/// Run a session's tracked agent, consuming `records` once it starts.
async fn run_session(
    ctx: &Context,
    workspace: SpaceId,
    agent: TrackedAgent,
    records: Vec<std::path::PathBuf>,
    command: Vec<std::ffi::OsString>,
) -> Result<i32> {
    if records.is_empty() {
        return crate::execution::run(
            &ctx.paths,
            SpaceSelector::Id(workspace),
            command,
            Some(agent),
        )
        .await;
    }
    crate::execution::run_recovery(
        &ctx.paths,
        SpaceSelector::Id(workspace),
        command,
        agent,
        records,
    )
    .await
}
