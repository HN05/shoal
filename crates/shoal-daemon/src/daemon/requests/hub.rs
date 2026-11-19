//! The daemon's answers to [`HubMethod`] requests, and who may send them:
//! anyone lists the hub's members and the spaces of its machines, as far as
//! their scope reaches them, and only the user changes the members, joins,
//! leaves or updates the hub.
use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::{hub::directory::HubView, space_names::SpaceSelector};

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use crate::{
    protocol::{Body, HubMethod},
    refusal::{Reason, Refusal},
};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: HubMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    if caller.is_some() && !matches!(method, HubMethod::HubMembers | HubMethod::HubDirectory) {
        return Err(Refusal::new(
            Reason::NotReady,
            "only the user's own terminal pairs, joins, leaves, updates and revokes the hub's machines",
        )
        .into());
    }
    Ok(Answer::Body(match method {
        HubMethod::PairHub => Body::Pairing(manager.hub.hub_pairing(&**manager).await?),
        HubMethod::JoinHub { via, name } => {
            Body::Membership(manager.hub.join_hub(&**manager, via, name).await?)
        }
        HubMethod::LeaveHub => Body::LeftHub(manager.hub.leave_hub().await?),
        HubMethod::UpdateHub => Body::HubUpdate(manager.hub.update_hub(&**manager).await?),
        HubMethod::HubMembers => Body::HubOverview(manager.hub.hub_overview(&**manager).await?),
        HubMethod::RevokeHubMember { member } => {
            Body::HubMember(manager.hub.revoke_hub_member(&**manager, member).await?)
        }
        HubMethod::HubDirectory => {
            let view = manager.hub.hub_view(&**manager, caller).await;
            let parts = view.map(|view| view.map(HubView::into_parts).unwrap_or_default());
            return Ok(Answer::pages(parts, Body::HubView));
        }
    }))
}

impl Scoped for HubMethod {
    async fn scope(
        &mut self,
        _manager: &Manager,
        _caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        // The hub's machines belong to no space.
        Ok(None)
    }
}
