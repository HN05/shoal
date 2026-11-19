//! A start of a new workspace, which the daemon carries out from one
//! [`Launch`] whoever makes it: this process reads only what needs it, such
//! as where the start is made and the session `--fork` forks, what a picker
//! on a terminal picks, and runs the agent where it runs in this caller.
use anyhow::Result;
use shoal_protocol::launches::{Launch, StartTarget};
use shoal_protocol::space_names::SpaceSelector;

use super::super::{fork_from, mode_repository};
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

/// The settings of the registered repository `args` name without asking,
/// else the global config's.
async fn launch_settings(ctx: &Context, args: &StartArgs) -> Result<Effective> {
    let target = match mode_repository(ctx, args).await? {
        Some(repo) => ConfigTarget::Repository(repo.id.into()),
        None => ConfigTarget::Global,
    };
    workspaces::DaemonReads(ctx).settings(target).await
}

/// `launch` in the `checked` repository, such as one the start registered,
/// in place of the positional argument that named it, beside a positional
/// issue or link.
pub(in crate::cli::commands::start) fn checked_launch(
    mut launch: Launch,
    checked: Option<&crate::repository::Selector>,
) -> Launch {
    if let Some(checked) = checked {
        if matches!(launch.target, Some(StartTarget::Repository(_))) {
            launch.target = None;
        }
        launch.repository = Some(checked.value().to_owned());
    }
    launch
}

/// Have the daemon start the new workspace `args` ask for in `mode`, which
/// a terminal-free start checks under the repository's settings.
pub(in crate::cli::commands::start) async fn start(
    ctx: &Context,
    args: StartArgs,
    mode: ModeChoice,
) -> Result<i32> {
    let settings = launch_settings(ctx, &args).await?;
    let mut launch = checked_launch(args.launch(), args.checked_repository.as_ref());
    let copied = copies::source(ctx, args.copy.clone()).await?;
    if let Some(copied) = &copied {
        copied.copy_into(&mut launch)?;
    }
    if args.fork {
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
    launch.parent = (launch.parent.take()).or(crate::env::own_chat().map(SpaceSelector::Id));
    launch.directory = ui::current_directory()?;
    launch.env = Some(crate::terminal::start_environment());
    launch.setup_env = Some(crate::env::without_secrets());
    let options = workspaces::AddOptions {
        here: args.here,
        focus: args.focus,
        wait: args.wait,
        ..Default::default()
    };
    workspaces::launch_workspace(ctx, launch, mode, (&options, &settings)).await
}
