//! Asking before a merge that brings a target sensitive files it changed
//! too, as the daemon recorded them.
use anyhow::{Result, ensure};

use crate::{
    cli::{context::Context, ui},
    model::ConfirmedWarning,
    protocol::ItemContext,
};

/// Ask whether to merge the PR `item` names in `context`, or the linked one,
/// despite its warning, unless it has none, and return what the user
/// confirmed, which the merge must still warn about.
pub(in crate::cli::commands) async fn confirm_pull(
    ctx: &Context,
    context: &ItemContext,
    item: Option<String>,
) -> Result<ConfirmedWarning> {
    let warning =
        crate::client::forge_reads::merge_warning(&ctx.paths, context.clone(), item).await?;
    if let Some(warning) = &warning {
        ensure!(
            ui::confirm(ctx, &warning.to_string(), "--yes")?,
            "merge canceled"
        );
    }
    Ok(ConfirmedWarning { warning })
}
