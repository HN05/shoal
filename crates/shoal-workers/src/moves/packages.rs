//! Where a move's packages go, and what it says it did with each.
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};

use crate::client::connection::CommandPaths;
use shoal_core::transfer::{
    Carrying, MoveDestination, MoveOrder, MoveReport, SpaceMoved, held_copies, package_name,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::RemovedSpace;
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
        MoveDestination::Ssh(_) | MoveDestination::Machine(_) | MoveDestination::Held => {
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

/// The file the package of a copy of the space `id`, or of its removal
/// record, which stays here too, goes to on its way to `destination`, as
/// [`reserve`] finds a move's, except that one held for the joined machine
/// that started the copy waits apart from the package of a move, refusing
/// a copy while an earlier one's package waits there.
pub(super) fn reserve_copy(
    paths: &CommandPaths,
    destination: &MoveDestination,
    id: &SpaceId,
) -> Result<PathBuf> {
    let MoveDestination::Held = destination else {
        return reserve(paths, destination, id);
    };
    let directory = held_copies(&paths.move_packages());
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("create {}", directory.display()))?;
    let file = directory.join(package_name(id));
    ensure!(
        !file.exists(),
        "a copy of it waits for the machine that started it already; copy it again once that machine has read it, or 30 minutes after it was written, when it goes"
    );
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
    let exporting =
        crate::client::transfer::export_removed(paths, selector, file.clone(), carrying.clone());
    let exported = exporting.await?;
    Ok(SpaceMoved {
        id: space.space_id.clone(),
        name: space.name.clone(),
        manifest: Some(exported.manifest),
        file: Some(file),
        removal: None,
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
    if let Some(machine) = other_machine(&order.destination) {
        return Some(arrived(&machine, order.keep, moved));
    }
    Some(match (&order.destination, &moved.file) {
        (MoveDestination::Client { .. } | MoveDestination::Held, _) => return None,
        (_, None) => format!("Exported {space}"),
        (_, Some(file)) => format!(
            "Exported {space} to {}; shoal restore --from opens it {opens}",
            file.display()
        ),
    })
}

/// The other machine `destination` takes packages to, as a move's lines
/// name it: its SSH host, or the joined machine; none for a file.
pub(super) fn other_machine(destination: &MoveDestination) -> Option<String> {
    match destination {
        MoveDestination::Ssh(destination) => Some(destination.host.clone()),
        MoveDestination::Machine(destination) => Some(format!("machine {}", destination.machine)),
        MoveDestination::File { .. }
        | MoveDestination::Directory { .. }
        | MoveDestination::Client { .. }
        | MoveDestination::Held => None,
    }
}

/// The line that tells where `moved` went on `host`, under the name the
/// import there gave it: a workspace's path there, and whether its agents
/// continue there, or for a removed space's record what opens it there.
fn arrived(host: &str, copy: bool, moved: &SpaceMoved) -> String {
    let name = &moved.name;
    // Only a removed space neither copies nor ends here.
    if !copy && moved.removal.is_none() && moved.arrived.is_none() {
        return format!(
            "Moved the record of {name} to {host}; shoal restore {} there opens it",
            moved.id
        );
    }
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
    let machine = other_machine(&order.destination);
    let verb = match (&machine, order.keep) {
        (Some(_), false) => "Moved",
        (Some(_), true) => "Copied",
        (None, false) => "Exported",
        (None, true) => "Exported copies of",
    };
    let to = match (&order.destination, machine) {
        (_, Some(machine)) => format!(" to {machine}"),
        (MoveDestination::Directory { path }, None) => format!(" to {}", path.display()),
        _ => String::new(),
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
