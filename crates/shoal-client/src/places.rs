//! Requests about the places a space's executions run in, and those Shoal
//! created for spaces.
use shoal_ids::space_ids::SpaceId;

use anyhow::Result;

use crate::connection::{Daemon, call, request};
use shoal_protocol::{
    model::{OwnedPlace, SpacePlaces},
    protocol::PlaceMethod,
    sessions::{Origin, PlaceClose, PlaceOwnership},
    space_names::SpaceSelector,
};

/// The places Shoal created for every space, a workspace or a chat.
pub async fn owned_places(daemon: &impl Daemon) -> Result<Vec<OwnedPlace>> {
    request(daemon, PlaceMethod::OwnedPlaces).await
}

/// Where the executions of `space`, a workspace or a chat, run, and the
/// places Shoal created for it.
pub async fn places(daemon: &impl Daemon, space: SpaceSelector) -> Result<SpacePlaces> {
    request(daemon, PlaceMethod::SpacePlaces { space }).await
}

/// Record `origin`, a place Shoal created for `workspace`, so the daemon
/// closes it once the workspace is removed.
pub async fn record_owned_place(
    daemon: &impl Daemon,
    workspace: &SpaceId,
    origin: Origin,
) -> Result<()> {
    call(
        daemon,
        PlaceMethod::RecordOwnedPlace {
            workspace: SpaceSelector::id(workspace),
            origin,
        },
    )
    .await?;
    Ok(())
}

/// Whose `place`, the place of a terminal host, is for `space`, as the
/// daemon knows it.
pub async fn place_ownership(
    daemon: &impl Daemon,
    space: &SpaceId,
    place: &Origin,
) -> Result<PlaceOwnership> {
    request(
        daemon,
        PlaceMethod::PlaceOwnership {
            space: SpaceSelector::id(space),
            place: place.clone(),
        },
    )
    .await
}

/// Close `place`, a place Shoal created for `workspace`, while the daemon
/// still records it as that workspace's.
pub async fn close_owned_place(
    daemon: &impl Daemon,
    workspace: &SpaceId,
    place: &Origin,
) -> Result<PlaceClose> {
    request(
        daemon,
        PlaceMethod::CloseOwnedPlace {
            workspace: workspace.clone(),
            place: place.clone(),
        },
    )
    .await
}
