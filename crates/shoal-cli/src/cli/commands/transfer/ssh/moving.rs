//! `shoal move ssh`: hand spaces to another machine's Shoal, ending each
//! here only once its import there succeeded, so a failed move leaves it
//! here with its agents running again.
use anyhow::{Context as _, Result};

use super::{
    super::export::{self, Checked, ExportDirectory, PlannedExport},
    ImportFailure, Remote, arrived, summary,
};
use crate::cli::commands::restore::agents::queue::{Stopped, restore_stopped, saved_executions};
use crate::client::execution_ids::ExecutionId;
use crate::client::output::transfer::{Imported, Moves};
use crate::client::space_names::SpaceSelector;
use crate::transfer::{ExportedSpace, Space};
use crate::{
    cli::{context::Context, ui},
    model::{CheckpointNumber, PresentSpace},
    removal::UnsavedWork,
};
use std::collections::BTreeSet;

/// Move `spaces` to `remote` once each one can end here, asking once where
/// ending them needs the user unless `yes`. Each space moves after those
/// below it, which its end here would otherwise take, and a space that
/// fails to move stays here without keeping the others.
pub(super) async fn move_spaces(
    ctx: &Context,
    remote: &Remote,
    spaces: &[PresentSpace],
    yes: bool,
) -> Result<i32> {
    let staging = tempfile::tempdir()?;
    let directory = ExportDirectory::create(staging.path().to_owned())?;
    let heading = match spaces {
        [space] => format!("Move {} to {}", space.name(), remote.host),
        _ => format!("Move {} spaces to {}", spaces.len(), remote.host),
    };
    let operation = ("move", "move");
    let mut planned =
        export::check_exports(ctx, spaces, &directory, yes, &heading, operation).await?;
    planned.sort_by_key(PlannedExport::below);
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

/// Export the planned space, which stops its agents, and import it on
/// `remote`, queueing its agents' restores or starting its chat there when
/// it resumes them, then end it here. When the export fails or the other
/// machine refuses the import, the space stays here and the agents its
/// export stopped restore here; when how the import ended is unknown, they
/// stay stopped, since the space may continue there.
async fn move_one(ctx: &Context, remote: &Remote, planned: &PlannedExport) -> Result<Imported> {
    let saved = saved_executions(ctx, planned.id()).await?;
    let checkpoint = match export_moving(ctx, planned).await {
        Ok(checkpoint) => checkpoint,
        Err(error) => return Err(stays_here(ctx, planned, saved, error).await),
    };
    let imported = match remote.import(&planned.file()).await {
        Ok(imported) => imported,
        Err(ImportFailure::Refused(error)) => {
            return Err(stays_here(ctx, planned, saved, error).await);
        }
        Err(ImportFailure::Unknown(error)) => {
            return Err(error.context(format!(
                "{host} may already have it, so its agents stay stopped here; if it arrived there, shoal rm {id} removes it here, and otherwise shoal restore {id} continues it here",
                host = remote.host,
                id = planned.id()
            )));
        }
    };
    end_moved(ctx, planned.checked(), checkpoint)
        .await
        .with_context(|| {
            format!(
                "it now exists here and on {}; once the work you want is there, shoal rm {} removes it here",
                remote.host,
                planned.id()
            )
        })?;
    Ok(imported)
}

/// `error`, which kept the planned space here, once the agents its export
/// stopped, those it saves beyond the `saved` before, restore here.
async fn stays_here(
    ctx: &Context,
    planned: &PlannedExport,
    saved: BTreeSet<ExecutionId>,
    error: anyhow::Error,
) -> anyhow::Error {
    restore_here(ctx, planned, saved).await;
    error.context("it stays here")
}

/// Stop the agents of the planned space and export it to its file, leaving
/// it here, returning the checkpoint a workspace's package holds.
async fn export_moving(ctx: &Context, planned: &PlannedExport) -> Result<Option<CheckpointNumber>> {
    let space = SpaceSelector::id(planned.id());
    let exported =
        crate::client::transfer::export_moving(&ctx.paths, space, planned.file()).await?;
    export::warn(&exported);
    Ok(exported_checkpoint(&exported))
}

/// The number of the checkpoint the package of a workspace holds.
fn exported_checkpoint(exported: &ExportedSpace) -> Option<CheckpointNumber> {
    match &exported.manifest.space {
        Space::Workspace(packaged) => Some(packaged.workspace.checkpoint.number),
        Space::Chat(_) => None,
    }
}

/// End the space `checked` describes, which the other machine imported
/// from its export at `checkpoint`.
async fn end_moved(
    ctx: &Context,
    checked: &Checked,
    checkpoint: Option<CheckpointNumber>,
) -> Result<()> {
    let removal = match checked {
        Checked::Workspace(check) => {
            let space = SpaceSelector::id(&check.workspace.id);
            let unsaved = UnsavedWork::confirming(check);
            let ended = crate::client::transfer::end_moved(&ctx.paths, space, unsaved, checkpoint);
            super::super::super::workspaces::leaving(ctx, &check.workspace, ended).await?
        }
        Checked::Chat(chat) => {
            let space = SpaceSelector::id(&chat.id);
            let unsaved = UnsavedWork::default();
            crate::client::transfer::end_moved(&ctx.paths, space, unsaved, None).await?
        }
    };
    export::warn_removal(removal.as_ref());
    Ok(())
}

/// Queue the restores of the agents the export of the planned space
/// stopped: those it saves now beyond the `saved` before the export.
async fn restore_here(ctx: &Context, planned: &PlannedExport, saved: BTreeSet<ExecutionId>) {
    let stopped = match saved_executions(ctx, planned.id()).await {
        Ok(now) => now.difference(&saved).cloned().collect(),
        Err(error) => {
            eprintln!("warning: cannot read the stopped agents; run shoal restore: {error:#}");
            return;
        }
    };
    restore_stopped(ctx, &Stopped::from([(planned.id().clone(), stopped)])).await;
}
