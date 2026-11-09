//! `shoal move export --keep`: write copies of spaces that keep running
//! here, which `shoal move import` opens as new spaces.
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};

use super::{
    super::STANDARD_STREAM, DirectoryReport, ExportDirectory, Output, ensure_unscoped_export,
    repository_spaces, selected_spaces, warn,
};
use crate::client::space_names::SpaceSelector;
use crate::model::CheckpointNumber;
use crate::{
    cli::{
        context::Context,
        ui::{self, Fallback},
    },
    model::PresentSpace,
    transfer::ExportedSpace,
};

/// `shoal move export --keep`: write a copy of the space `names` names, or
/// the current one or one a picker selects, to `file`, leaving it running: a
/// workspace at its checkpoint `checkpoint`, or at a new one. Several spaces,
/// or any written to a directory, go to a file each in it.
pub(in super::super) async fn export_copy(
    ctx: &Context,
    names: Vec<SpaceSelector>,
    file: PathBuf,
    checkpoint: Option<CheckpointNumber>,
) -> Result<i32> {
    ensure_unscoped_export(ctx)?;
    if names.len() > 1 || file.is_dir() {
        ensure!(
            file != Path::new(STANDARD_STREAM),
            "several copies export to a file each; name a directory with -o"
        );
        let spaces = selected_spaces(ctx, names).await?;
        return copy_into(ctx, &spaces, file, checkpoint).await;
    }
    let output = Output::reserve(&file)?;
    let space = ui::select_space(ctx, names.into_iter().next(), Fallback::CurrentDirectory).await?;
    let exported = copy(ctx, space, checkpoint, output.file()).await?;
    output.report(ctx, &exported)
}

/// `shoal move export --all --keep`: write a copy of every space of
/// `repository` to a file of its own in `directory`.
pub(in super::super) async fn export_all_copies(
    ctx: &Context,
    repository: String,
    directory: PathBuf,
) -> Result<i32> {
    ensure_unscoped_export(ctx)?;
    let spaces = repository_spaces(ctx, repository).await?;
    copy_into(ctx, &spaces, directory, None).await
}

/// Write a copy of each of `spaces` to a file of its own in `directory`,
/// going on after one fails, since a copy removes nothing.
async fn copy_into(
    ctx: &Context,
    spaces: &[PresentSpace],
    directory: PathBuf,
    checkpoint: Option<CheckpointNumber>,
) -> Result<i32> {
    ensure_one_at_checkpoint(spaces.len(), checkpoint)?;
    let directory = ExportDirectory::create(directory)?;
    let mut report = DirectoryReport::new(&directory, true);
    for space in spaces {
        let copied = copy_one(ctx, &directory, space, checkpoint).await;
        report.add(ctx, space.name(), copied);
    }
    report.finish(ctx)
}

/// Write a copy of `space` to its file in `directory`, refusing a file
/// that exists.
async fn copy_one(
    ctx: &Context,
    directory: &ExportDirectory,
    space: &PresentSpace,
    checkpoint: Option<CheckpointNumber>,
) -> Result<ExportedSpace> {
    let output = Output::reserve(&directory.file(space.name()))?;
    copy(
        ctx,
        SpaceSelector::id(space.id()),
        checkpoint,
        output.file(),
    )
    .await
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

/// Write a copy of `space` to `file`, at the workspace's checkpoint
/// `checkpoint` or a new one, warning about what the copy lacks.
pub(in super::super) async fn copy(
    ctx: &Context,
    space: SpaceSelector,
    checkpoint: Option<CheckpointNumber>,
    file: PathBuf,
) -> Result<ExportedSpace> {
    let exported = crate::client::transfer::copy(&ctx.paths, space, checkpoint, file).await?;
    warn(&exported);
    if let Some(number) = checkpoint {
        eprintln!(
            "warning: the agents' conversations go past checkpoint {number} and describe work its files do not contain"
        );
    }
    Ok(exported)
}
