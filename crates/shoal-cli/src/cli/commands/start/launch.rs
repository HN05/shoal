//! A start of a new workspace that does not run in the caller's terminal,
//! which the daemon carries out from one [`Launch`]: this process reads
//! only what needs it, such as where the start is made and the session
//! `--fork` forks, and what a picker on a terminal picks.
use anyhow::Result;
use shoal_protocol::space_names::SpaceSelector;

use super::{fork_from, mode_repository};
use crate::{
    cli::{
        args::create::StartArgs,
        commands::{copies, repositories, workspaces},
        context::Context,
        ui::{self, ModeChoice},
    },
    config::Effective,
    launches::LaunchReads,
    protocol::ConfigTarget,
};

/// The settings of the repository the start `args` ask for names, when the
/// daemon carries it out: a start not run here, for a scoped caller, whose
/// own processes its agent must not be, or without a terminal to start it
/// beside, unless the user's agent runs its own way, which starts in this
/// process.
pub(super) async fn detached(ctx: &Context, args: &StartArgs) -> Result<Option<Effective>> {
    if args.here || !(crate::env::is_scoped() || !ctx.on_terminal()) {
        return Ok(None);
    }
    let settings = launch_settings(ctx, args).await?;
    let agent = (args.agent.clone()).or_else(|| settings.agent.default.clone());
    let own_way = !crate::env::is_scoped()
        && agent.is_some_and(|agent| crate::cli::agents::runs_its_own_way(&agent, &settings));
    Ok((!own_way).then_some(settings))
}

/// The settings of the registered repository `args` name without asking,
/// else the global config's.
async fn launch_settings(ctx: &Context, args: &StartArgs) -> Result<Effective> {
    let target = match mode_repository(ctx, args).await? {
        Some(repo) => ConfigTarget::Repository(repo.id.into()),
        None => ConfigTarget::Global,
    };
    workspaces::DaemonReads(ctx).settings(target).await
}

/// Have the daemon start the new workspace `args` ask for in `mode`, which
/// a terminal-free start checks under the repository's `settings`.
pub(super) async fn workspace(
    ctx: &Context,
    args: StartArgs,
    mode: ModeChoice,
    settings: Effective,
) -> Result<i32> {
    let mut launch = args.launch();
    if args.fork {
        let copied = copies::source(ctx, args.copy.clone()).await?;
        let copied = copied.map(|copy| copy.source.workspace);
        let (parent, agent, fork) =
            fork_from(ctx, copied, args.parent.clone(), args.agent.clone()).await?;
        (launch.parent, launch.agent, launch.fork) = (parent, agent, fork);
    }
    launch.task = workspaces::launch_args(args.args.clone())?;
    launch.path = (launch.path.take())
        .map(|path| repositories::absolute(ctx, path))
        .transpose()?;
    // A start in a chat puts its workspace below the chat.
    launch.parent = (launch.parent.take()).or(crate::env::in_chat().map(SpaceSelector::Id));
    launch.directory = ui::current_directory()?;
    launch.env = Some(crate::terminal::start_environment());
    launch.setup_env = Some(crate::env::without_secrets());
    let options = workspaces::AddOptions {
        focus: args.focus,
        ..Default::default()
    };
    workspaces::launch_workspace(ctx, launch, mode, (&options, &settings)).await
}
