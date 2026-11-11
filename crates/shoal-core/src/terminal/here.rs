//! The place this process runs in, as its host names it, and what tells it
//! apart from a later place under the same ID.
use anyhow::Result;

use super::{Host, Places, hosts};
use crate::sessions::Origin;

/// The place this process runs in, as the host serving it names it.
pub fn current_origin() -> Option<Origin> {
    hosts().find_map(origin_here)
}

/// The place of `host` this process runs in, as `host` names it, with what
/// a later start needs to open a place beside it.
pub fn origin_here(host: &dyn Host) -> Option<Origin> {
    host.places().and_then(Places::origin)
}

/// The place this process runs in, as [`current_origin`] names it, with what
/// tells it apart from a later place under its ID, for the daemon's record of
/// where an execution or chat runs. Without that, a record left by a process
/// that crashed would name a later place under the ID. A host that cannot
/// tell leaves it out, with a warning.
pub async fn current_place() -> Option<Origin> {
    for host in hosts() {
        if let Some(origin) = origin_here(host) {
            return Some(match with_instance(host, origin.clone()).await {
                Ok(place) => place,
                Err(error) => {
                    eprintln!(
                        "warning: cannot tell this {} place apart: {error:#}",
                        host.name()
                    );
                    origin
                }
            });
        }
    }
    None
}

/// `place` of `host` with what tells it apart from a later place under its
/// ID, for a host that reuses IDs.
pub async fn with_instance(host: &dyn Host, mut place: Origin) -> Result<Origin> {
    if let Some(watches) = host.watches() {
        place.instance = watches.place_instance(place.id()).await?;
    }
    Ok(place)
}
