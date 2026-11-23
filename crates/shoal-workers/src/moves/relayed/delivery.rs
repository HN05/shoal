//! Delivering a package another joined machine holds: the new file it goes
//! to, created before anything ends there, the package read into it over
//! the hub, and the end of the space there once the file holds it.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

use crate::client::connection::CommandPaths;
use shoal_core::transfer::{MoveDestination, MoveOrder, SpaceMoved, package_name};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

/// The new file the package of one space goes to, created before anything
/// ends.
pub(super) struct Reserved {
    pub(super) id: SpaceId,
    path: PathBuf,
    file: std::fs::File,
}

impl Reserved {
    /// The new file of the package of each space of `ids` that
    /// `destination` names, creating none when one cannot be created.
    pub(super) fn all(destination: &MoveDestination, ids: &[SpaceId]) -> Result<Vec<Self>> {
        let mut reserved = Vec::new();
        for id in ids {
            match Self::new(destination, id) {
                Ok(file) => reserved.push(file),
                Err(error) => {
                    reserved.into_iter().for_each(Self::remove);
                    return Err(error);
                }
            }
        }
        Ok(reserved)
    }

    /// The new file of the package of the space `id` that `destination`
    /// names: the file, or its file in the directory, which is created.
    fn new(destination: &MoveDestination, id: &SpaceId) -> Result<Self> {
        let path = match destination {
            MoveDestination::File { path } | MoveDestination::Client { path } => path.clone(),
            MoveDestination::Directory { path } => {
                std::fs::create_dir_all(path)
                    .with_context(|| format!("create the directory {}", path.display()))?;
                path.join(package_name(id))
            }
            _ => bail!("only a move to files reads its packages from another machine"),
        };
        let file = std::fs::File::create_new(&path)
            .with_context(|| format!("create {}; choose another file", path.display()))?;
        let id = id.clone();
        Ok(Self { id, path, file })
    }

    /// Delete the file, which takes no package.
    pub(super) fn remove(self) {
        let _ = std::fs::remove_file(&self.path);
    }

    /// [`Self::remove`] the file, which takes no package for `error`.
    fn discard(self, error: anyhow::Error) -> anyhow::Error {
        self.remove();
        error
    }
}

/// Write the package of `packed`, which its machine holds, into `reserved`,
/// and end the space there unless it `stays` there, as a copy's space or a
/// removed space's record does, or `order` streams its file to the
/// command, which ends it once copied.
pub(super) async fn deliver(
    paths: &CommandPaths,
    order: &MoveOrder,
    (mut packed, stays): (SpaceMoved, bool),
    reserved: Reserved,
) -> Result<SpaceMoved> {
    let selector = SpaceSelector::id(&packed.id);
    let file = take_package(paths, selector.clone(), stays, reserved).await?;
    let streamed = matches!(order.destination, MoveDestination::Client { .. });
    if !stays && !streamed {
        let ending = crate::client::transfer::end_moved(paths, selector).await;
        packed.removal = Some(ending.with_context(|| both_places(&file, &packed.id))?);
    }
    packed.file = Some(file);
    Ok(packed)
}

/// Write the package the machine of the space `space` holds into
/// `reserved`, a `copy`'s, which a removed space's is, or a move's; one
/// that does not arrive whole
/// leaves no file, and gives a move back there, where the space stays with
/// its agents' restores queued.
async fn take_package(
    paths: &CommandPaths,
    space: SpaceSelector,
    copy: bool,
    mut reserved: Reserved,
) -> Result<PathBuf> {
    let reading = crate::client::transfer::read_held_package;
    let Err(error) = reading(paths, space.clone(), copy, &mut reserved.file).await else {
        return Ok(reserved.path);
    };
    if !copy && let Err(released) = crate::client::transfer::release_held_move(paths, space).await {
        super::super::warn(&format!("warning: {released:#}"));
    }
    let error = error.context("its package did not arrive, so it stays where it runs");
    Err(reserved.discard(error))
}

/// What a move of the space `id`, whose package went to `file`, says when
/// the space could not end where it runs after all.
fn both_places(file: &Path, id: &SpaceId) -> String {
    format!(
        "it now exists where it runs and in {}; once the work you want is there, shoal rm {id} removes it there",
        file.display()
    )
}
