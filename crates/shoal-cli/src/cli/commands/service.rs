//! CLI OS-service administration, including offline/protocol-upgrade handling.
mod config;
mod daemon;
mod install;

use anyhow::Result;

pub(super) use config::{install_config, packaged_config};
pub(super) use daemon::status as shoal_status;

use crate::cli::{args::service::DaemonCommand, context::Context, entry::Daemon};

pub(super) async fn run(ctx: Context, command: DaemonCommand, daemon: Daemon) -> Result<i32> {
    match command {
        DaemonCommand::Install {
            dry_run,
            executable,
        } => install::install(&ctx, dry_run, executable).await,
        DaemonCommand::Run { managed } => {
            (daemon.run)(ctx.paths.into(), managed, daemon.handoff).await?;
            Ok(0)
        }
        DaemonCommand::Start => daemon::start(&ctx).await,
        DaemonCommand::Stop => daemon::stop(&ctx).await,
        DaemonCommand::Restart => daemon::restart(&ctx).await,
    }
}
