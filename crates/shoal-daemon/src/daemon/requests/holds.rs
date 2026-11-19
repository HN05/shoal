//! The daemon's answers to [`HoldMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::{
    model::HoldPlacer,
    protocol::{Body, HoldMethod},
};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: HoldMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    // A scoped process acts for its agent, anyone else for the user.
    let placer = match caller {
        Some(_) => HoldPlacer::Agent,
        None => HoldPlacer::User,
    };
    Ok(Answer::Body(match method {
        // An agent asks to hold its own space.
        HoldMethod::HoldAcquire {
            space,
            name,
            reason,
        } if caller.is_some_and(|caller| SpaceSelector::id(&caller.space_id) == space) => manager
            .request_hold(&space, name, reason)
            .await?
            .into_body(Body::Hold),
        HoldMethod::HoldAcquire {
            space,
            name,
            reason,
        } => Body::Hold(manager.acquire_hold(&space, name, reason, placer).await?),
        HoldMethod::HoldRelease { space, name } => {
            manager.release_hold_as(caller, &space, name).await?;
            Body::Ok
        }
        HoldMethod::HoldList { space } => Body::Holds(manager.holds(&space).await?),
    }))
}

impl Scoped for HoldMethod {
    /// Holding the caller's own space needs approval, which
    /// [`handle`] requests.
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut SpaceSelector>> {
        Ok(Some(match self {
            HoldMethod::HoldList { space }
            | HoldMethod::HoldRelease { space, .. }
            | HoldMethod::HoldAcquire { space, .. } => space,
        }))
    }
}
