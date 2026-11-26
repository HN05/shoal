//! A start on another joined machine that `--here` or `--focus` shows in this
//! terminal, which attaches to the agent through that machine.
use anyhow::{Result, ensure};
use shoal_protocol::launches::Launch;
use shoal_protocol::protocol::{Launched, Started};

use super::StartOptions;
use crate::cli::{
    commands::{links, preparation, sessions},
    context::Context,
};

pub(in crate::cli::commands) fn shown_elsewhere(launch: &Launch, options: &StartOptions) -> bool {
    launch.machine.is_some() && (options.here || options.focus)
}

/// A scoped caller is refused, as `shoal attach` refuses it.
pub(in crate::cli::commands) async fn start(ctx: &Context, mut launch: Launch) -> Result<i32> {
    ensure!(
        !crate::process::scoped::inherits_scope(&ctx.paths.state),
        "--here and --focus attach your terminal to the agent on machine {}, as shoal attach does, which workspace processes do not; leave them out to start it there in the background",
        launch.machine.as_deref().unwrap_or_default()
    );
    launch.now = true;
    if !ctx.on_terminal() {
        return super::attach::send(ctx, launch, (false, None), None).await;
    }
    let launched = crate::client::launches::launch(&ctx.paths, launch).await?;
    let (space, machine) = (launched.space.id.clone(), launched.machine.clone());
    if let Some(code) = await_preparation(ctx, launched, machine.as_deref()).await? {
        return Ok(code);
    }
    sessions::attach_started(ctx, space, machine.as_deref()).await
}

async fn await_preparation(
    ctx: &Context,
    launched: Launched,
    machine: Option<&str>,
) -> Result<Option<i32>> {
    match &launched.start {
        Started::Preparing {
            operation: Some(_), ..
        } => {
            let started = preparation::workspace_start(launched)?;
            preparation::prepared_on(ctx, started, machine).await?;
            Ok(None)
        }
        Started::Preparing {
            operation: None, ..
        } => preparation::report_launched(ctx, launched, (false, None))
            .await
            .map(Some),
        Started::Queued { .. } | Started::Worker | Started::Detached(_) => {
            links::print_warnings(&launched.warnings);
            Ok(None)
        }
    }
}
