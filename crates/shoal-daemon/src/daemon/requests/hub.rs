//! The daemon's answers to [`HubMethod`] requests, and who may send them:
//! anyone lists the hub's members and the spaces of its machines, as far as
//! their scope reaches them, a space updates the hub as its `daemon`
//! permission allows, only the user changes the members, joins or
//! leaves, and only an unscoped client serves as the placer.
use shoal_daemon_base::permitted_actions::Action;
use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::space_names::SpaceSelector;
use tokio::net::UnixStream;

use super::super::{
    answer::{Answer, OperationGuard},
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::{
    config::permissions::PermissionKey,
    protocol::{self, Body, HubMethod, Response},
    refusal::{Reason, Refusal},
};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: HubMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    authorize(manager, &method, caller).await?;
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
        HubMethod::ServePlacement => {
            let registration = manager.hub.serve_placement(&**manager)?;
            return Ok(Answer::connection(move |stream, request_id, guard| {
                serve_placement(stream, request_id, registration, guard)
            }));
        }
        HubMethod::AnswerPlacement { number, placement } => {
            manager.hub.answer_placement(number, placement)?;
            Body::Ok
        }
    }))
}

/// Send the placer `registration` on `stream` each start the hub asks it to
/// place, once it heard that it registered, until it hangs up or a later
/// registration takes over.
async fn serve_placement(
    mut stream: UnixStream,
    request_id: u64,
    mut registration: shoal_hub::Registration,
    startup: OperationGuard,
) -> Result<()> {
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    drop(startup);
    let (mut reader, mut writer) = stream.split();
    let mut closed = [0u8; 1];
    loop {
        tokio::select! {
            asked = registration.asked.recv() => {
                let Some(asked) = asked else {
                    return Ok(());
                };
                let response = Response::new(request_id, Body::PlacementAsked(asked));
                protocol::write(&mut writer, &response).await?;
            }
            // The placer answers on connections of their own; a read
            // completes only when it hangs up.
            _ = tokio::io::AsyncReadExt::read(&mut reader, &mut closed) => return Ok(()),
        }
    }
}

/// Refuse `caller` what `method` does that its space may not.
async fn authorize(manager: &Manager, method: &HubMethod, caller: Option<&Caller>) -> Result<()> {
    let Some(caller) = caller else {
        return Ok(());
    };
    match method {
        HubMethod::HubMembers => Ok(()),
        HubMethod::ServePlacement | HubMethod::AnswerPlacement { .. } => Err(Refusal::new(
            Reason::NotReady,
            "only an unscoped client, such as an app's server, registers as the placer and answers for it",
        )
        .into()),
        HubMethod::UpdateHub => {
            let action = Action::new(PermissionKey::Daemon, "shoal hub update");
            manager.authorize_action(Some(caller), action).await
        }
        _ => Err(Refusal::new(
            Reason::NotReady,
            "only the user's own terminal pairs, joins, leaves and revokes the hub's machines",
        )
        .into()),
    }
}

impl Checked for HubMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::PairHub
            | Self::JoinHub { .. }
            | Self::LeaveHub
            | Self::HubMembers
            | Self::UpdateHub
            | Self::RevokeHubMember { .. }
            | Self::ServePlacement
            | Self::AnswerPlacement { .. } => None,
        }
    }
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

#[cfg(test)]
mod tests;
