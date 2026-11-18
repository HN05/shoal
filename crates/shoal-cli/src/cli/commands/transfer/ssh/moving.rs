//! `shoal move ssh`: hand spaces to another machine's Shoal, ending each
//! here only once its import there succeeded, so a failed move leaves it
//! here.
use anyhow::{Context as _, Result};

use super::{
    super::export::{self, Ending, ExportDirectory, PlannedExport},
    ImportFailure, Remote, arrived, summary,
};
use crate::cli::commands::restore::agents::queue::{Stopped, restore_stopped, saved_executions};
use crate::client::output::{restore::Reopened, transfer::Moves};
use crate::transfer::{Carrying, ExportedSpace, ManifestExt};
use crate::{
    cli::{context::Context, ui},
    model::Space,
    removal::UnsavedWork,
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use std::collections::{BTreeMap, BTreeSet};

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
    let saved = saved_now(ctx, planned).await?;
    let checkpoints = match export_moving(ctx, planned).await {
        Ok(checkpoints) => checkpoints,
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
    end_moved(ctx, planned, checkpoints)
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
    saved: Stopped,
    error: anyhow::Error,
) -> anyhow::Error {
    restore_here(ctx, planned, saved).await;
    error.context("it stays here")
}

/// Stop the agents of the planned space and of the spaces it carries and
/// export them to its file, leaving them here, returning the checkpoint
/// each workspace's package holds.
async fn export_moving(ctx: &Context, planned: &PlannedExport) -> Result<Checkpoints> {
    let space = SpaceSelector::id(planned.id());
    let carrying = planned.carrying().clone();
    let exporting =
        crate::client::transfer::export_moving(&ctx.paths, space, planned.file(), carrying);
    let exported = exporting.await?;
    export::warn(&exported);
    Ok(exported_checkpoints(&exported))
}

/// The checkpoints a package holds, by the ID of each workspace it holds.
type Checkpoints = BTreeMap<SpaceId, CheckpointNumber>;

/// The number of the checkpoint the package holds of each workspace.
fn exported_checkpoints(exported: &ExportedSpace) -> Checkpoints {
    (exported.manifest.packaged())
        .filter_map(|packaged| {
            let kept = packaged.record.workspace()?;
            Some((packaged.record.space_id.clone(), kept.checkpoint.number))
        })
        .collect()
}

/// End the `planned` space with the spaces it carries, which the other
/// machine imported from its export at `checkpoints`.
async fn end_moved(ctx: &Context, planned: &PlannedExport, checkpoints: Checkpoints) -> Result<()> {
    let (checked, carrying) = (planned.checked(), planned.carrying().clone());
    let removal = match &checked.space {
        Ending::Workspace(check) => {
            let space = SpaceSelector::id(&check.space.id);
            let unsaved = UnsavedWork::confirming(check);
            let end = crate::client::transfer::end_moved;
            let ended = end(&ctx.paths, space, unsaved, checkpoints, carrying);
            super::super::super::workspaces::leaving(ctx, &check.space, ended).await?
        }
        Ending::Chat(chat) => {
            let space = SpaceSelector::id(&chat.id);
            let unsaved = checked.chat_ending();
            let end = crate::client::transfer::end_moved;
            end(&ctx.paths, space, unsaved, checkpoints, carrying).await?
        }
    };
    export::warn_removal(Some(&removal));
    Ok(())
}

/// The executions whose stopped work the planned space, and each space
/// that goes with it, saves now.
async fn saved_now(ctx: &Context, planned: &PlannedExport) -> Result<Stopped> {
    let mut saved = Stopped::new();
    for space in planned.checked().spaces() {
        saved.insert(space.clone(), saved_executions(ctx, space).await?);
    }
    Ok(saved)
}

/// Queue the restores of the agents the export of the planned space
/// stopped, its own and those of the spaces that go with it: those they
/// save now beyond the `saved` before the export.
async fn restore_here(ctx: &Context, planned: &PlannedExport, saved: Stopped) {
    let now = match saved_now(ctx, planned).await {
        Ok(now) => now,
        Err(error) => {
            eprintln!("warning: cannot read the stopped agents; run shoal restore: {error:#}");
            return;
        }
    };
    let stopped = (now.into_iter())
        .map(|(space, now)| {
            let before = saved.get(&space).cloned().unwrap_or_default();
            let stopped: BTreeSet<ExecutionId> = now.difference(&before).cloned().collect();
            (space, stopped)
        })
        .collect();
    restore_stopped(ctx, &stopped).await;
}
