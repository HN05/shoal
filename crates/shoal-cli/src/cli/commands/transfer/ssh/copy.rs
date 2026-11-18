//! `shoal move ssh --keep`: copy spaces to another machine's Shoal, with
//! copies of the spaces below them, which opens each as a new space,
//! leaving them running here.
use anyhow::Result;
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_protocol::space_names::SpaceSelector;

use super::{super::export, Remote, arrived, summary};
use crate::client::output::{restore::Reopened, transfer::Copies};
use crate::{
    cli::{context::Context, ui},
    model::Space,
    transfer::Carrying,
};

/// Copy each of `spaces`, with copies of the spaces below it that
/// `carrying` carries, to `remote`, the workspace at its checkpoint
/// `checkpoint` or a new one, queueing the copies' agents there when it
/// resumes them. A copy removes nothing, so one that fails leaves the rest
/// to copy.
pub(super) async fn copy_spaces(
    ctx: &Context,
    remote: &Remote,
    spaces: &[Space],
    checkpoint: Option<CheckpointNumber>,
    carrying: &Carrying,
) -> Result<i32> {
    let staging = tempfile::tempdir()?;
    let directory = export::ExportDirectory::create(staging.path().to_owned())?;
    let spaces = export::copy::copied_alone(ctx, spaces, carrying).await?;
    let mut report = ui::Report::new("copy");
    for space in &spaces {
        let name = &space.name;
        let copying = Copying {
            directory: &directory,
            checkpoint,
            carrying,
        };
        match copy_one(ctx, remote, space, copying).await {
            Ok(imported) => {
                let copy = imported.space.name.as_str();
                let message = copied_message(name, copy, &arrived(&remote.host, &imported));
                report.done(ctx, &message, imported)
            }
            Err(error) => report.failed(ctx, name, &error),
        }
    }
    let summary = summary("Copied", &remote.host, report.counts());
    report.finish(ctx, &summary, |copied, failed| Copies { copied, failed })
}

/// How a batch copies each space: to a file in `directory`, at the
/// checkpoint `checkpoint` or a new one, with the spaces below it that
/// `carrying` carries.
struct Copying<'a> {
    directory: &'a export::ExportDirectory,
    checkpoint: Option<CheckpointNumber>,
    carrying: &'a Carrying,
}

/// Write a copy of `space` as `copying` says, then import it on `remote`,
/// queueing its agents' restores there when it resumes them.
async fn copy_one(
    ctx: &Context,
    remote: &Remote,
    space: &Space,
    copying: Copying<'_>,
) -> Result<Reopened> {
    let file = copying.directory.file(&space.id);
    let selector = SpaceSelector::id(&space.id);
    let carrying = copying.carrying.clone();
    export::copy::copy(ctx, selector, copying.checkpoint, file.clone(), carrying).await?;
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
