//! `shoal move ssh --keep`: copy spaces to another machine's Shoal, which
//! opens each as a new space, leaving them running here.
use crate::client::space_names::SpaceSelector;
use anyhow::Result;
use shoal_ids::checkpoint_numbers::CheckpointNumber;

use super::{super::export, Remote, arrived, summary};
use crate::client::output::{restore::Reopened, transfer::Copies};
use crate::{
    cli::{context::Context, ui},
    model::PresentSpace,
};

/// Copy each of `spaces` to `remote`, the workspace at its checkpoint
/// `checkpoint` or a new one, queueing the copies' agents there when it
/// resumes them. A copy removes nothing, so one that fails leaves the rest
/// to copy.
pub(super) async fn copy_spaces(
    ctx: &Context,
    remote: &Remote,
    spaces: &[PresentSpace],
    checkpoint: Option<CheckpointNumber>,
) -> Result<i32> {
    let staging = tempfile::tempdir()?;
    let directory = export::ExportDirectory::create(staging.path().to_owned())?;
    let mut report = ui::Report::new("copy");
    for space in spaces {
        let name = space.name();
        match copy_one(ctx, remote, &directory, space, checkpoint).await {
            Ok(imported) => {
                let copy = imported.space.name().as_str();
                let message = copied_message(name, copy, &arrived(&remote.host, &imported));
                report.done(ctx, &message, imported)
            }
            Err(error) => report.failed(ctx, name, &error),
        }
    }
    let summary = summary("Copied", &remote.host, report.counts());
    report.finish(ctx, &summary, |copied, failed| Copies { copied, failed })
}

/// Write a copy of `space` to its file in `directory`, then import it on
/// `remote`, queueing its agents' restores there when it resumes them.
async fn copy_one(
    ctx: &Context,
    remote: &Remote,
    directory: &export::ExportDirectory,
    space: &PresentSpace,
    checkpoint: Option<CheckpointNumber>,
) -> Result<Reopened> {
    let file = directory.file(space.id());
    export::copy::copy(ctx, SpaceSelector::id(space.id()), checkpoint, file.clone()).await?;
    Ok(remote.import(&file).await?)
}

/// The report of copying `name` as `copy` to where it `arrived`, naming the
/// copy when the import there gave it another name.
fn copied_message(name: &str, copy: &str, arrived: &str) -> String {
    match copy == name {
        true => format!("Copied {name} to {arrived}"),
        false => format!("Copied {name} as {copy} to {arrived}"),
    }
}

#[cfg(test)]
mod tests;
