//! `shoal move export --keep`: write a copy of a space that keeps running
//! here, which `shoal move import` opens as a new space.
use std::path::PathBuf;

use anyhow::{Result, ensure};

use super::{Output, ensure_unscoped_export, warn};
use crate::cli::{
    context::Context,
    ui::{self, Fallback},
};

/// `shoal move export --keep`: write a copy of the space `names` names, or
/// the current one or one a picker selects, to `file`, leaving it running: a
/// workspace at its checkpoint `checkpoint`, or at a new one.
pub(in super::super) async fn export_copy(
    ctx: &Context,
    names: Vec<String>,
    file: PathBuf,
    checkpoint: Option<u64>,
) -> Result<i32> {
    ensure_unscoped_export(ctx)?;
    ensure!(names.len() <= 1, "--keep exports a copy of one space");
    let output = Output::reserve(&file)?;
    let space = ui::select_space(ctx, names.into_iter().next(), Fallback::CurrentDirectory).await?;
    let exported =
        crate::client::transfer::copy(&ctx.paths, space, checkpoint, output.file()).await?;
    warn(&exported);
    if let Some(number) = checkpoint {
        eprintln!(
            "warning: the agents' conversations go past checkpoint {number} and describe work its files do not contain"
        );
    }
    output.report(ctx, &exported)
}
