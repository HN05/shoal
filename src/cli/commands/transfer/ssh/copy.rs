//! `shoal move ssh --keep`: copy spaces to another machine's Shoal, which
//! opens each as a new space, leaving them running here.
use anyhow::Result;

use super::{super::export, Remote, arrived, summary};
use crate::client::output::transfer::{Copies, Imported};
use crate::{
    cli::{context::Context, ui},
    model::PresentSpace,
};

/// Copy each of `spaces` to `remote`, the workspace at its checkpoint
/// `checkpoint` or a new one, queueing the copies' agents there when
/// `resume`. A copy removes nothing, so one that fails leaves the rest to
/// copy.
pub(super) async fn copy_spaces(
    ctx: &Context,
    remote: &Remote,
    spaces: &[PresentSpace],
    checkpoint: Option<u64>,
    resume: bool,
) -> Result<i32> {
    let staging = tempfile::tempdir()?;
    let directory = export::ExportDirectory::create(staging.path().to_owned())?;
    let mut report = ui::Report::new("copy");
    for space in spaces {
        let name = space.name();
        match copy_one(ctx, remote, &directory, space, checkpoint, resume).await {
            Ok(imported) => {
                let message = copied_message(name, &remote.host, &imported);
                report.done(ctx, &message, imported)
            }
            Err(error) => report.failed(ctx, name, &error),
        }
    }
    let summary = summary("Copied", &remote.host, report.counts());
    report.finish(ctx, &summary, |copied, failed| Copies { copied, failed })
}

/// Write a copy of `space` to its file in `directory`, then import it on
/// `remote`, queueing its agents' restores or starting its chat there when
/// `resume`.
async fn copy_one(
    ctx: &Context,
    remote: &Remote,
    directory: &export::ExportDirectory,
    space: &PresentSpace,
    checkpoint: Option<u64>,
    resume: bool,
) -> Result<Imported> {
    let file = directory.file(space.name());
    export::copy::copy(ctx, space.id().to_owned(), checkpoint, file.clone()).await?;
    remote.import(&file, resume).await
}

/// The report of copying `name` to `host`, naming the copy when the import
/// there gave it another name.
fn copied_message(name: &str, host: &str, imported: &Imported) -> String {
    let copy = match imported {
        Imported::Workspace(reopened) => reopened.workspace.name.as_str(),
        Imported::Chat(chat) => chat.chat.as_str(),
    };
    match copy == name {
        true => format!("Copied {name} to {}", arrived(host, imported)),
        false => format!("Copied {name} as {copy} to {}", arrived(host, imported)),
    }
}

#[cfg(test)]
mod tests;
