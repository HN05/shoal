//! `shoal start`: start an agent in a mode, which decides what the agent is
//! told, whether it starts in a repository and whether it starts in a
//! workspace or a chat.
//!
//! Every start settles its launch through one function, sends it, and runs
//! the agent of the space the daemon recorded through one placement: here,
//! beside this terminal, in a background place or detached. A chat's agent
//! talks with the user about a repository in its registered checkout,
//! about a workspace's work in its worktree, or without a repository in
//! `root_dir`.
use anyhow::{Result, ensure};

use crate::cli::{args::create::StartArgs, context::Context};

/// Start what `args` ask for.
pub(super) async fn run(ctx: &Context, args: StartArgs) -> Result<i32> {
    ensure_scope_token(ctx)?;
    settle::start(ctx, args).await
}

/// Refuse a workspace process without its scope token, which the daemon
/// would take for the user's: a start would create a space outside its own,
/// and a request would reach agents outside its scope.
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
mod run;
mod settle;
mod shown;

pub(in crate::cli) use attach::execute_place;
pub(in crate::cli::commands) use attach::{Placed, place, run as run_attached};
pub(in crate::cli::commands) use detached::{leaves_caller, place_agent};
pub(in crate::cli::commands) use placement::{Attach, StartOptions};
pub(in crate::cli::commands) use reads::{
    launch_repository, launch_settings, space_repository, start_modes, target_settings,
};
pub(in crate::cli::commands) use reports::report_queued_in;
pub(in crate::cli) use run::run_entry;
pub(in crate::cli::commands) use settle::{
    check_model, check_start_arguments, launch_args, pick_start_source, settle,
};

#[cfg(test)]
mod tests;
