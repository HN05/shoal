//! CLI OS-service administration, including offline/protocol-upgrade handling.
mod config;
mod daemon;
mod install;

use anyhow::Result;

pub(super) use config::{install_config, packaged_config};

use crate::cli::{args::service::DaemonCommand, context::Context};

pub(super) async fn run(
    ctx: Context,
    command: DaemonCommand,
    handoff: Option<crate::daemon::handoff::Handoff>,
) -> Result<i32> {
    match command {
        DaemonCommand::Install {
            dry_run,
            executable,
        } => install::install(&ctx, dry_run, executable).await,
        DaemonCommand::Run { managed } => {
            crate::daemon::run(ctx.paths, managed, handoff).await?;
            Ok(0)
        }
        DaemonCommand::Status => daemon::status(&ctx).await,
        DaemonCommand::Start => daemon::start(&ctx).await,
        DaemonCommand::Stop => daemon::stop(&ctx).await,
        DaemonCommand::Reload => daemon::reload(&ctx).await,
        DaemonCommand::Restart => daemon::restart(&ctx).await,
    }
}
