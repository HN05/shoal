//! The daemon's answers to [`PlaceMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::{Result, ensure};

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped, denied},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{Body, PlaceMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(manager: &Arc<Manager>, method: PlaceMethod) -> Result<Answer> {
    Ok(Answer::Body(match method {
        PlaceMethod::CloseOwnedPlace { workspace, place } => {
            Body::PlaceClose(manager.close_recorded_place(&workspace, &place).await?)
        }
        PlaceMethod::PlaceOwnership { space, place } => {
            Body::PlaceOwnership(manager.place_ownership(&space, &place).await?)
        }
        PlaceMethod::OwnedPlaces => owned_places(manager).await?,
        PlaceMethod::SpacePlaces { space } => {
            Body::SpacePlaces(manager.space_places(&space).await?)
        }
        PlaceMethod::RecordOwnedPlace { workspace, origin } => {
            manager.record_owned_place(&workspace, origin).await?;
            Body::Ok
        }
    }))
}

/// The places Shoal created for every space.
async fn owned_places(manager: &Manager) -> Result<Body> {
    Ok(Body::OwnedPlaces(
        manager
            .owned_places(None)
            .await?
            .into_iter()
            .map(|(space_id, origin)| crate::model::OwnedPlace { space_id, origin })
            .collect(),
    ))
}

impl Checked for PlaceMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::SpacePlaces { .. }
            | Self::RecordOwnedPlace { .. }
            | Self::PlaceOwnership { .. } => Some(TargetCheck::Space),
            Self::OwnedPlaces | Self::CloseOwnedPlace { .. } => None,
        }
    }
}

impl Scoped for PlaceMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            // The workspace may be removed already, so its ID is checked as
            // given instead of resolved.
            PlaceMethod::CloseOwnedPlace { workspace, .. } => {
                ensure!(
                    manager.reaches(caller, workspace).await?,
                    "workspace processes close only the places of their own space and those below it"
                );
                None
            }
            PlaceMethod::SpacePlaces { space } | PlaceMethod::PlaceOwnership { space, .. } => {
                Some(space)
            }
            PlaceMethod::RecordOwnedPlace { workspace, .. } => Some(workspace),
            PlaceMethod::OwnedPlaces => return Err(denied()),
        })
    }
}
