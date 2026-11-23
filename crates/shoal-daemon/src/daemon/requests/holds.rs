//! The daemon's answers to [`HoldMethod`] requests, who may send them, and what
//! each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
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
            .spaces
            .request_hold(&**manager, &space, name, reason)
            .await?
            .into_body(Body::Hold),
        HoldMethod::HoldAcquire {
            space,
            name,
            reason,
        } => Body::Hold(
            manager
                .spaces
                .acquire_hold(&**manager, &space, name, reason, placer)
                .await?,
        ),
        HoldMethod::HoldRelease { space, name } => {
            manager
                .spaces
                .release_hold_as(&**manager, caller, &space, name)
                .await?;
            Body::Ok
        }
        HoldMethod::HoldList { space } => Body::Holds(manager.spaces.holds(&space).await?),
    }))
}

impl Checked for HoldMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::HoldAcquire { .. } | Self::HoldList { .. } => Some(TargetCheck::Space),
            Self::HoldRelease { .. } => Some(TargetCheck::HeldSpace),
        }
    }
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
