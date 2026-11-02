//! Asking before a merge that brings a target sensitive files it changed
//! too, as the daemon recorded them.
use anyhow::{Result, ensure};

use crate::{
    cli::{context::Context, ui},
    model::MergeWarning,
};

/// Ask whether to merge despite `warning`, unless there is none.
pub(in crate::cli::commands) fn confirm(
    ctx: &Context,
    warning: Option<MergeWarning>,
) -> Result<()> {
    let Some(warning) = warning else {
        return Ok(());
    };
    ensure!(
        ui::confirm(ctx, &warning.to_string(), "--yes")?,
        "merge canceled"
    );
    Ok(())
}
