//! The `shoal restore` tests drive: it records each workspace whose last
//! session a relocation continued, for the daemon that continued it.
use shoal_ids::space_ids::SpaceId;

use crate::{daemon::per_daemon::PerDaemon, paths::Paths};

static RESUMED: PerDaemon<SpaceId> = PerDaemon::new();

pub(super) fn restore(paths: &Paths, id: &SpaceId) {
    RESUMED.push(paths, id.clone());
}

/// The workspaces whose last session a relocation of the daemon of `paths`
/// continued.
#[cfg(test)]
pub(crate) fn resumed(paths: &Paths) -> Vec<SpaceId> {
    RESUMED.of(paths)
}
