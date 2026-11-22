//! The launch a start's arguments ask for, made where the start is made:
//! the spaces it names by ID, the copy it copies, the session `--fork`
//! forks, its task, and the terminal it is made in.
use std::ffi::OsString;

use anyhow::Result;
use shoal_ids::{agent_session_ids::AgentSessionId, space_ids::SpaceId};
use shoal_protocol::launches::Launch;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    agent::Agent,
    cli::{
        args::create::StartArgs,
        commands::{copies, repositories},
        context::Context,
        ui::{self, Fallback},
    },
    model::AgentSession,
    protocol::ConfigTarget,
};

/// The launch `args` ask for, made from this process: below the space
/// they name, in the workspace `--in` names or picks, with the copy and
/// the session `--fork` forks it reads, the agent's task, below the chat
/// it runs in, in its directory and with its terminal's environment.
pub(super) async fn launch(ctx: &Context, args: &StartArgs) -> Result<Launch> {
    let mut launch = args.launch();
    if let Some(parent) = &launch.parent {
        launch.parent = Some(SpaceSelector::Id(space_id(ctx, parent).await?));
    }
    if let Some(workspace) = args.in_workspace.clone() {
        let selected = ui::select_workspace(
            ctx,
            workspace,
            Fallback::CurrentDirectory(&crate::cli::ui::Acting::Unconverted),
        )
        .await?;
        launch.in_workspace = Some(Some(selected).into());
    }
    let copied = copies::source(ctx, args.copy.clone()).await?;
    if let Some(copied) = &copied {
        copied.copy_into(&mut launch)?;
    }
    if args.fork {
        let copied = copied.map(|copy| copy.source.workspace);
        forked_into(ctx, &mut launch, copied).await?;
    }
    launch.task = launch_args(args.args.clone())?;
    launch.path = (launch.path.take())
        .map(|path| repositories::absolute(ctx, path))
        .transpose()?;
    // A start in a chat puts its new space below the chat; one in an
    // existing workspace creates none.
    if launch.in_workspace.is_none() {
        launch.parent = (launch.parent.take()).or(crate::env::own_chat().map(SpaceSelector::Id));
    }
    launch.directory = ui::current_directory()?;
    launch.env = Some(crate::terminal::start_environment());
    launch.setup_env = Some(crate::env::without_secrets());
    Ok(launch)
}

/// Agent arguments as a queued launch carries them, refusing one that is
/// not UTF-8.
pub(in crate::cli::commands) fn launch_args(args: Vec<OsString>) -> Result<Vec<String>> {
    (args.into_iter())
        .map(|arg| {
            arg.into_string()
                .map_err(|arg| anyhow::anyhow!("agent argument {arg:?} is not UTF-8"))
        })
        .collect()
}

/// The ID of the space `selector` names, by number or ID: one of this
/// machine, else one the hub lists on another joined machine.
async fn space_id(ctx: &Context, selector: &SpaceSelector) -> Result<SpaceId> {
    let located = crate::client::spaces::locate(&ctx.paths, selector.clone()).await?;
    Ok(located.id().clone())
}

/// `launch` started from a fork of the session of the agent `--fork`
/// forks: the `copied` workspace's, whose copy goes below the parent
/// `launch` names as any start does; else the agent of the existing
/// workspace `launch` starts in; else the parent's, which the start then
/// goes below.
async fn forked_into(
    ctx: &Context,
    launch: &mut Launch,
    copied: Option<SpaceSelector>,
) -> Result<()> {
    let in_workspace = (launch.in_workspace.as_ref()).and_then(|choice| choice.named().cloned());
    let (space, keeps_parent) = match (copied, in_workspace) {
        (Some(copied), _) => (Some(copied), true),
        (None, Some(_)) => (None, true),
        (None, None) => (launch.parent.clone(), false),
    };
    let (forked, agent, session) = forked(ctx, space, launch.agent.take()).await?;
    if !keeps_parent {
        launch.parent = Some(forked);
    }
    (launch.agent, launch.fork) = (Some(agent), Some(session));
    Ok(())
}

/// The space, a workspace or a chat, whose agent `--fork` forks, `space`
/// or else the current one, the agent that starts from the fork, `agent` or
/// else that one, and the session it forks. An agent that cannot start from
/// a fork is refused before anything is created.
async fn forked(
    ctx: &Context,
    space: Option<SpaceSelector>,
    agent: Option<Agent>,
) -> Result<(SpaceSelector, Agent, AgentSessionId)> {
    let space = ui::select_space(
        ctx,
        space,
        Fallback::CurrentDirectory(&crate::cli::ui::Acting::Unconverted),
    )
    .await?;
    let AgentSession {
        agent: running,
        session,
        ..
    } = crate::client::agents::session(&ctx.paths, space.clone()).await?;
    let agent = match agent {
        Some(agent) => agent,
        None => running.name().parse()?,
    };
    let target = ConfigTarget::Space(space.clone());
    let settings = shoal_workers::settings::of(&ctx.paths, target).await?;
    crate::agents::ensure_forks(&settings, &agent)?;
    Ok((space, agent, session))
}
