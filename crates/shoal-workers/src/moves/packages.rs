//! Where a move's packages go, and what it says it did with each: a new
//! file, a file per space in a directory, a file its client copies to its
//! standard output, or a package delivered from here to another machine.
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};

use crate::client::connection::CommandPaths;
use shoal_core::transfer::{Carrying, MoveDestination, MoveOrder, MoveReport, SpaceMoved};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::RemovedSpace;
use shoal_protocol::removal::UnsavedWork;
use shoal_protocol::space_names::SpaceSelector;

/// The file the package of the space `id` goes to on its way to
/// `destination`, refusing a file that exists or a directory that takes
/// none, before anything ends; a directory to write a file per space to is
/// created.
pub(super) fn reserve(
    paths: &CommandPaths,
    destination: &MoveDestination,
    id: &SpaceId,
) -> Result<PathBuf> {
    let file = destination.package(id, &paths.move_packages());
    let directory = match destination {
        MoveDestination::Directory { path } => {
            std::fs::create_dir_all(path)
                .with_context(|| format!("create the directory {}", path.display()))?;
            path.as_path()
        }
        MoveDestination::Ssh(_) => {
            let staging = paths.move_packages();
            std::fs::create_dir_all(&staging)
                .with_context(|| format!("create {}", staging.display()))?;
            return Ok(file);
        }
        MoveDestination::File { .. } | MoveDestination::Client { .. } => {
            shoal_core::transfer::directory(&file)
        }
    };
    ensure!(
        !file.exists(),
        "{} exists; choose another file",
        file.display()
    );
    tempfile::NamedTempFile::new_in(directory)
        .with_context(|| format!("create a file in {}", directory.display()))?;
    Ok(file)
}

/// Write the removed space `space`, which needs no end here, with the
/// spaces its removal took that `carrying` carries, to `file`.
pub(super) async fn export_removed(
    paths: &CommandPaths,
    space: &RemovedSpace,
    file: PathBuf,
    carrying: &Carrying,
) -> Result<SpaceMoved> {
    let selector = SpaceSelector::id(&space.space_id);
    let unsaved = UnsavedWork::default();
    let exporting =
        crate::client::transfer::export(paths, selector, unsaved, file.clone(), carrying.clone());
    let exported = exporting.await?;
    Ok(SpaceMoved {
        id: space.space_id.clone(),
        name: space.name.clone(),
        manifest: exported.manifest,
        file: Some(file),
        removal: exported.removal,
        arrived: None,
        warnings: exported.warnings,
    })
}

/// The line that tells what the move `order` did with the space `moved`;
/// none for a package its client copies to its standard output, which
/// tells it once copied.
pub(super) fn done(order: &MoveOrder, moved: &SpaceMoved) -> Option<String> {
    let name = &moved.name;
    let space = match order.keep {
        true => format!("a copy of {name}"),
        false => name.to_string(),
    };
    let opens = match order.keep {
        true => "as a new space",
        false => "on another machine",
    };
    Some(match (&order.destination, &moved.file) {
        (MoveDestination::Ssh(destination), _) => arrived(&destination.host, order.keep, moved),
        (MoveDestination::Client { .. }, _) => return None,
        (_, None) => format!("Exported {space}"),
        (_, Some(file)) => format!(
            "Exported {space} to {}; shoal restore --from opens it {opens}",
            file.display()
        ),
    })
}

/// The line that tells where `moved` went on `host`, under the name the
/// import there gave it: a workspace's path there, and whether its agents
/// continue there.
fn arrived(host: &str, copy: bool, moved: &SpaceMoved) -> String {
    let name = &moved.name;
    let mut message = match (copy, &moved.arrived) {
        (true, Some(arrived)) if arrived.space.name != *name => {
            format!("Copied {name} as {} to {host}", arrived.space.name)
        }
        (true, _) => format!("Copied {name} to {host}"),
        (false, _) => format!("Moved {name} to {host}"),
    };
    let Some(arrived) = &moved.arrived else {
        return message;
    };
    if let Some(workspace) = arrived.space.worktree() {
        message.push_str(&format!(" at {}", workspace.path.display()));
    }
    match arrived.queued.len() {
        0 => {}
        1 => message.push_str("; its agent continues there"),
        _ => message.push_str("; its agents continue there"),
    }
    message
}

/// The line that sums up a move of several spaces, or one that failed.
pub(super) fn summary(order: &MoveOrder, report: &MoveReport) -> String {
    let (done, failed) = (report.moved.len(), report.failed.len());
    let verb = match (&order.destination, order.keep) {
        (MoveDestination::Ssh(_), false) => "Moved",
        (MoveDestination::Ssh(_), true) => "Copied",
        (_, false) => "Exported",
        (_, true) => "Exported copies of",
    };
    let to = match &order.destination {
        MoveDestination::Ssh(destination) => format!(" to {}", destination.host),
        MoveDestination::Directory { path } => format!(" to {}", path.display()),
        MoveDestination::File { .. } | MoveDestination::Client { .. } => String::new(),
    };
    if failed > 0 {
        return format!(
            "{verb} {done} of {} spaces{to}; {failed} failed",
            done + failed
        );
    }
    let mut summary = format!("{verb} {done} spaces{to}");
    if let MoveDestination::Directory { path } = &order.destination {
        let opens = match order.keep {
            true => "as new spaces",
            false => "on another machine",
        };
        summary.push_str(&format!(
            "; shoal restore --from {} opens them {opens}",
            path.display()
        ));
    }
    summary
}

#[cfg(test)]
mod tests;
