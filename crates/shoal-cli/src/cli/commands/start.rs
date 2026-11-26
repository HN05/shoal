//! `shoal start`. Every start settles its launch through one function, sends
//! it, and runs the agent of the space the daemon recorded through one
//! placement.
use anyhow::{Result, ensure};

use crate::cli::{args::create::StartArgs, context::Context};

pub(super) async fn run(ctx: &Context, args: StartArgs) -> Result<i32> {
    ensure_scope_token(ctx)?;
    settle::start(ctx, args).await
}

/// The daemon would take a process without its scope token for the user: a
/// start would create a space outside its own.
pub(super) fn ensure_scope_token(ctx: &Context) -> Result<()> {
    ensure!(
        crate::env::is_scoped() || !crate::process::scoped::inherits_scope(&ctx.paths.state),
        "workspace processes start and ask agents only with their scope token"
    );
    Ok(())
}

mod attach;
mod detached;
pub(in crate::cli::commands) mod placement;
mod reads;
mod reports;
mod settle;
mod shown;

pub(in crate::cli::commands) use attach::place;
pub(in crate::cli::commands) use detached::{leaves_caller, place_agent};
pub(in crate::cli::commands) use placement::{StartOptions, attach};
pub(in crate::cli::commands) use reads::{
    launch_repository, launch_settings, space_repository, start_modes, target_settings,
};
pub(in crate::cli::commands) use reports::report_queued_in;
pub(in crate::cli::commands) use settle::{
    check_model, check_start_arguments, launch_args, pick_start_source, settle, starting,
};

#[cfg(test)]
mod tests;
