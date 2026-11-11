//! Asking before a merge that brings a target sensitive files it changed
//! too, as the daemon recorded them.
use anyhow::{Result, ensure};

use crate::{
    cli::{context::Context, ui},
    model::MergeWarning,
};

/// Ask whether to merge despite `warning`, unless there is none, and return
/// the URL of the PR the user confirmed merging.
pub(in crate::cli::commands) fn confirm(
    ctx: &Context,
    warning: Option<MergeWarning>,
) -> Result<Option<String>> {
    let Some(warning) = warning else {
        return Ok(None);
    };
    ensure!(
        ui::confirm(ctx, &warning.to_string(), "--yes")?,
        "merge canceled"
    );
    Ok(warning.url)
}
