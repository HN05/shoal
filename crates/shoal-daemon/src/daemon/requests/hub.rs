//! The daemon's answers to [`HubMethod`] requests, and who may send them:
//! anyone lists the hub's members, and only the user changes them, joins or
//! leaves.
use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::space_names::SpaceSelector;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
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
    if caller.is_some() && !matches!(method, HubMethod::HubMembers) {
        return Err(Refusal::new(
            Reason::NotReady,
            "only the user's own terminal pairs, joins, leaves and revokes the hub's machines",
        )
        .into());
    }
    Ok(Answer::Body(match method {
        HubMethod::PairHub => Body::Pairing(manager.hub_pairing().await?),
        HubMethod::HubMembers => Body::HubOverview(manager.hub_overview().await?),
        HubMethod::RevokeHubMember { member } => {
            Body::HubMember(manager.revoke_hub_member(member).await?)
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
