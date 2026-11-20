//! `shoal move --ssh`: hand spaces to another machine's Shoal, ending each
//! here only once its import there succeeded, so a failed move leaves it
//! here.
use anyhow::{Context as _, Result};

use super::{
    super::export::{self, Ending, ExportDirectory, PlannedExport},
    ImportFailure, Remote, arrived, summary,
};
use crate::cli::commands::resume::queue::restore_stopped;
use crate::client::output::{restore::Reopened, transfer::Moves};
use crate::transfer::Carrying;
use crate::{
    cli::{context::Context, ui},
    model::Space,
    removal::UnsavedWork,
};
use shoal_protocol::space_names::SpaceSelector;
use std::path::PathBuf;

/// Move `spaces` with the spaces below them that `carrying` carries to
/// `remote` once each one can end here, asking once where ending them
/// needs the user unless `yes`. A space below another moves with it, and a
/// space that fails to move stays here without keeping the others.
pub(super) async fn move_spaces(
    ctx: &Context,
    remote: &Remote,
    spaces: &[Space],
    yes: bool,
    carrying: &Carrying,
) -> Result<i32> {
    let staging = tempfile::tempdir()?;
    let directory = ExportDirectory::create(staging.path().to_owned())?;
    let heading = match spaces {
        [space] => format!("Move {} to {}", space.name, remote.host),
        _ => format!("Move {} spaces to {}", spaces.len(), remote.host),
    };
    let batch = export::Batch {
        operation: ("move", "move"),
        heading: &heading,
        carrying,
    };
    let planned = export::check_exports(ctx, spaces, &directory, yes, batch).await?;
    let mut report = ui::Report::new("move");
    for planned in planned {
        let name = planned.name().to_owned();
        match move_one(ctx, remote, &planned).await {
            Ok(imported) => {
                let message = format!("Moved {name} to {}", arrived(&remote.host, &imported));
                report.done(ctx, &message, imported)
            }
            Err(error) => report.failed(ctx, &name, &error),
        }
    }
    let summary = summary("Moved", &remote.host, report.counts());
    report.finish(ctx, &summary, |moved, failed| Moves { moved, failed })
}

/// Export the planned space with the spaces it carries, which stops their
/// agents, and import them on `remote`, queueing their agents' restores
/// there when it resumes them, then end them here. When the export fails or the other
/// machine refuses the import, the space stays here and the agents its
/// export stopped restore here; when how the import ended is unknown, they
/// stay stopped, since the space may continue there.
async fn move_one(ctx: &Context, remote: &Remote, planned: &PlannedExport) -> Result<Reopened> {
    let file = match export_moving(ctx, remote, planned).await {
        Ok(file) => file,
        Err(error) => return Err(stays_here(ctx, planned, error).await),
    };
    let imported = match remote.import(&file).await {
        Ok(imported) => imported,
        Err(ImportFailure::Refused(error)) => {
            return Err(stays_here(ctx, planned, error).await);
        }
        Err(ImportFailure::Unknown(error)) => {
            let space = SpaceSelector::id(planned.id());
            let _ = crate::client::transfer::abandon_move(&ctx.paths, space).await;
            return Err(error.context(format!(
                "{host} may already have it, so its agents stay stopped here; if it arrived there, shoal rm {id} removes it here, and otherwise shoal resume {id} continues it here",
                host = remote.host,
                id = planned.id()
            )));
        }
    };
    end_moved(ctx, planned).await.with_context(|| {
        format!(
            "it now exists here and on {}; once the work you want is there, shoal rm {} removes it here",
            remote.host,
            planned.id()
        )
    })?;
    Ok(imported)
}

/// `error`, which kept the planned space here, once the move is forgotten
/// and the agents its export stopped restore here.
async fn stays_here(ctx: &Context, planned: &PlannedExport, error: anyhow::Error) -> anyhow::Error {
    let space = SpaceSelector::id(planned.id());
    match crate::client::transfer::abandon_move(&ctx.paths, space).await {
        Ok(stopped) => {
            restore_stopped(ctx, &stopped).await;
        }
        Err(abandoned) => eprintln!("warning: {abandoned:#}"),
    }
    error.context("it stays here")
}

/// Record the planned space's move to `remote`, stop the agents of the
/// space and of the spaces it carries and export them, leaving them here,
/// returning the package's file.
async fn export_moving(ctx: &Context, remote: &Remote, planned: &PlannedExport) -> Result<PathBuf> {
    let space = SpaceSelector::id(planned.id());
    let unsaved = match &planned.checked().space {
        Ending::Workspace(check) => UnsavedWork::confirming(check),
        Ending::Chat(_) => planned.checked().chat_ending(),
    };
    let destination = remote.destination();
    let file = destination.package(planned.id(), &ctx.paths.move_packages());
    let carrying = planned.carrying().clone();
    let exporting =
        crate::client::transfer::export_moving(&ctx.paths, space, unsaved, destination, carrying);
    let exported = exporting.await?;
    export::warn(&exported);
    Ok(file)
}

/// End the `planned` space with the spaces it carries, which the other
/// machine imported.
async fn end_moved(ctx: &Context, planned: &PlannedExport) -> Result<()> {
    let space = SpaceSelector::id(planned.id());
    let end = crate::client::transfer::end_moved(&ctx.paths, space);
    let removal = match &planned.checked().space {
        Ending::Workspace(check) => {
            super::super::super::workspaces::leaving(ctx, &check.space, end).await?
        }
        Ending::Chat(_) => end.await?,
    };
    export::warn_removal(Some(&removal));
    Ok(())
}
