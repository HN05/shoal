//! The place this process runs in, as its host names it, and what tells it
//! apart from a later place under the same ID. A process in a session nested
//! in another host's place runs in that place, with the session nested in it.
use anyhow::Result;

use super::{Host, Places, hosts, named};
use crate::{
    env,
    sessions::{HostName, Origin},
};

/// The place this process runs in, as the host serving it names it.
pub fn current_origin() -> Option<Origin> {
    let (outer, nested) = places_here();
    nest(
        outer.map(|(_, origin)| origin),
        nested.map(|(_, origin)| origin),
    )
}

/// The place of `host` this process runs in, as `host` names it, with what
/// a later start needs to open a place beside it.
pub fn origin_here(host: &dyn Host) -> Option<Origin> {
    host.places().and_then(Places::origin)
}

/// The place this process runs in, as [`current_origin`] names it, with what
/// tells it and its nested session apart from later ones under their IDs,
/// for the daemon's record of where an execution or chat runs. Without that,
/// a record left by a process that crashed would name a later place under
/// the ID. A host that cannot tell leaves it out, with a warning.
pub async fn current_place() -> Option<Origin> {
    let (outer, nested) = places_here();
    let outer = match outer {
        Some((host, origin)) => Some(told_apart(host, origin).await),
        None => None,
    };
    let nested = match nested {
        Some((host, origin)) => Some(told_apart(host, origin).await),
        None => None,
    };
    nest(outer, nested)
}

/// The host whose session this process runs in, nested in a place of
/// another host, as the launch that nested it says.
pub(super) fn nested_host() -> Option<HostName> {
    std::env::var(env::NESTED_SESSION).ok()?.parse().ok()
}

/// A place this process runs in, with the host that names it.
type Hosted = (&'static dyn Host, Origin);

/// The place this process runs in, outside any session nested in it, and
/// that session.
fn places_here() -> (Option<Hosted>, Option<Hosted>) {
    let nested = nested_host().map(named);
    let outer = hosts()
        .filter(|host| nested.is_none_or(|nested| nested.name() != host.name()))
        .find_map(|host| Some((host, origin_here(host)?)));
    let nested = nested.and_then(|host| Some((host, origin_here(host)?)));
    (outer, nested)
}

/// `outer` with `nested` nested in it, or `nested` alone when this process
/// names no place outside it.
fn nest(outer: Option<Origin>, nested: Option<Origin>) -> Option<Origin> {
    match outer {
        Some(outer) => Some(Origin {
            nested: nested.map(Box::new),
            ..outer
        }),
        None => nested,
    }
}

/// `place` of `host` with what tells it apart from a later place under its
/// ID, or as it is, with a warning, when the host cannot tell.
async fn told_apart(host: &dyn Host, place: Origin) -> Origin {
    match with_instance(host, place.clone()).await {
        Ok(place) => place,
        Err(error) => {
            eprintln!(
                "warning: cannot tell this {} place apart: {error:#}",
                host.name()
            );
            place
        }
    }
}

/// `place` of `host` with what tells it apart from a later place under its
/// ID, for a host that reuses IDs.
pub async fn with_instance(host: &dyn Host, mut place: Origin) -> Result<Origin> {
    if let Some(watches) = host.watches() {
        place.instance = watches.place_instance(place.id()).await?;
    }
    Ok(place)
}

#[cfg(test)]
mod tests;
