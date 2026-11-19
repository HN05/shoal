//! `shoal move export --keep`: write copies of spaces that keep running
//! here, with copies of the spaces below them, which `shoal restore --from`
//! opens as new spaces.
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};

use super::{
    super::STANDARD_STREAM, DirectoryReport, ExportDirectory, Output, selected_spaces, warn,
};
use crate::{
    cli::{
        context::Context,
        ui::{self, Fallback},
    },
    model::Space,
    transfer::{Carrying, ExportedSpace},
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_protocol::space_names::SpaceSelector;

/// `shoal move export --keep`: write a copy of the space `names` names, or
/// the current one or one a picker selects, to `file`, leaving it running: a
/// workspace at its checkpoint `checkpoint`, or at a new one. Several spaces,
/// or any written to a directory, go to a file each in it.
pub(in super::super) async fn export_copy(
    ctx: &Context,
    names: Vec<SpaceSelector>,
    file: PathBuf,
    checkpoint: Option<CheckpointNumber>,
    no_children: bool,
) -> Result<i32> {
    if names.len() > 1 || file.is_dir() {
        ensure!(
            file != Path::new(STANDARD_STREAM),
            "several copies export to a file each; name a directory with -o"
        );
        let spaces = selected_spaces(ctx, names).await?;
        let carrying = super::carrying(no_children, &spaces);
        return copy_into(ctx, &spaces, file, checkpoint, &carrying).await;
    }
    let output = Output::reserve(&file)?;
    let space = ui::select_space(ctx, names.into_iter().next(), Fallback::CurrentDirectory).await?;
    let carrying = super::carrying(no_children, &[]);
    let exported = copy(ctx, space, checkpoint, output.file(), carrying).await?;
    output.report(ctx, &exported)
}

/// Write a copy of each of `spaces`, with copies of the spaces below it
/// that `carrying` carries, to a file of its own in `directory`, a space
/// below another in its file, going on after one fails, since a copy
/// removes nothing.
pub(in super::super) async fn copy_into(
    ctx: &Context,
    spaces: &[Space],
    directory: PathBuf,
    checkpoint: Option<CheckpointNumber>,
    carrying: &Carrying,
) -> Result<i32> {
    let spaces = copied_alone(ctx, spaces, carrying).await?;
    ensure_one_at_checkpoint(spaces.len(), checkpoint)?;
    let directory = ExportDirectory::create(directory)?;
    let mut report = DirectoryReport::new(&directory, true);
    for space in &spaces {
        let copied = copy_one(ctx, &directory, space, checkpoint, carrying).await;
        report.add(ctx, (&space.name, &space.id), copied);
    }
    report.finish(ctx)
}

/// The `spaces` a copy of them all copies to a file each: those that do
/// not go with another of them as `carrying` says.
pub(in super::super) async fn copied_alone(
    ctx: &Context,
    spaces: &[Space],
    carrying: &Carrying,
) -> Result<Vec<Space>> {
    let tree = crate::client::spaces::list(&ctx.paths).await?;
    Ok(super::checks::exported_alone(spaces, &tree, carrying))
}

/// Write a copy of `space`, with copies of the spaces below it that
/// `carrying` carries, to its file in `directory`, refusing a file that
/// exists.
async fn copy_one(
    ctx: &Context,
    directory: &ExportDirectory,
    space: &Space,
    checkpoint: Option<CheckpointNumber>,
    carrying: &Carrying,
) -> Result<ExportedSpace> {
    let output = Output::reserve(&directory.file(&space.id))?;
    let selector = SpaceSelector::id(&space.id);
    copy(ctx, selector, checkpoint, output.file(), carrying.clone()).await
}

/// Refuse `--checkpoint` for `count` spaces other than one, since its
/// number names a checkpoint of one workspace.
pub(in super::super) fn ensure_one_at_checkpoint(
    count: usize,
    checkpoint: Option<CheckpointNumber>,
) -> Result<()> {
    ensure!(
        checkpoint.is_none() || count == 1,
        "--checkpoint copies one workspace, whose checkpoints it numbers; name one"
    );
    Ok(())
}

/// Write a copy of `space`, with copies of the spaces below it that
/// `carrying` carries, to `file`, at the workspace's checkpoint
/// `checkpoint` or a new one, warning about what the copy lacks.
pub(in super::super) async fn copy(
    ctx: &Context,
    space: SpaceSelector,
    checkpoint: Option<CheckpointNumber>,
    file: PathBuf,
    carrying: Carrying,
) -> Result<ExportedSpace> {
    let copying = crate::client::transfer::copy(&ctx.paths, space, checkpoint, file, carrying);
    let exported = copying.await?;
    warn(&exported);
    if let Some(number) = checkpoint {
        eprintln!(
            "warning: the agents' conversations go past checkpoint {number} and describe work its files do not contain"
        );
    }
    Ok(exported)
}
