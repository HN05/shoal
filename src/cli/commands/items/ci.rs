//! `shoal pr ci`: rerun or cancel a PR's CI jobs.
use anyhow::Result;

use crate::{
    cli::{args::changes::CiCommand, client, context::Context, ui},
    forge::ci::{CiActed, CiAction},
    protocol::CiMethod,
};

pub(super) async fn ci(ctx: &Context, command: CiCommand) -> Result<i32> {
    let (action, checks) = match command {
        CiCommand::Rerun { checks } => (CiAction::Rerun, checks),
        CiCommand::Cancel { checks } => (CiAction::Cancel, checks),
    };
    let workspace =
        ui::select_workspace(ctx, checks.item.workspace, ui::Fallback::CurrentDirectory).await?;
    let acted: CiActed = client::request(
        &ctx.paths,
        CiMethod::CiAction {
            workspace,
            item: checks.item.item,
            action,
            checks: checks.checks,
        },
    )
    .await?;
    let outcome = match action {
        CiAction::Rerun => "Rerun started",
        CiAction::Cancel => "Cancelled",
    };
    ctx.emit(
        &format!("{outcome} for {}: {}", acted.checks.join(", "), acted.url),
        &acted,
    )?;
    Ok(0)
}
