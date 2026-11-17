//! What the hub answers, to its members' calls and to the requests of its
//! own machine's callers alike: pairing codes, its listener and members, and
//! revoking credentials.
use anyhow::{Result, anyhow};
use shoal_protocol::hub::{
    Member, Overview,
    link::{Call, Reply},
    pairing::Pairing,
};

use super::members;
use crate::{
    daemon::workspace::Manager,
    refusal::{Reason, Refusal},
};

impl Manager {
    /// The hub's reply to `member`'s `call`.
    pub(super) async fn answer_member(&self, member: &Member, call: Call) -> Result<Reply> {
        Ok(match call {
            Call::Pair => Reply::Pairing(self.issue_pairing().await?),
            Call::Members => Reply::Members(self.local_hub_overview().await?),
            Call::Revoke { member } => Reply::Revoked(self.revoke_member(&member).await?),
            // The member closes its link once it reads the reply.
            Call::Leave => {
                let id = member.id.clone();
                self.store.write(move |tx| members::revoke(tx, &id)).await?;
                Reply::Left
            }
        })
    }

    /// The listener and members of this daemon as the hub.
    pub(super) async fn local_hub_overview(&self) -> Result<Overview> {
        self.own_member().await?;
        let own = self.hub_identity().await?.public.clone();
        let mut members = self.store.read(move |tx| members::list(tx, &own)).await?;
        let connected = self.hub.links.connected();
        for member in &mut members {
            member.connected = member.hub || connected.contains(&member.id);
            if member.hub {
                member.version = Some(env!("CARGO_PKG_VERSION").into());
                member.last_seen = None;
            }
        }
        Ok(Overview {
            listener: self.hub_listener(),
            members,
        })
    }

    /// Revoke the credential of the member `selector` names, ending its
    /// link.
    pub(super) async fn revoke_member(&self, selector: &str) -> Result<Member> {
        let own = self.hub_identity().await?.public.clone();
        let selector = selector.to_owned();
        let member = self
            .store
            .write(move |tx| {
                let member = members::select(tx, &selector, &own)?;
                if member.hub {
                    return Err(Refusal::new(
                        Reason::NotReady,
                        "the hub's own machine holds no credential; set [machine.hub] enabled = false to end the hub role",
                    )
                    .into());
                }
                members::revoke(tx, &member.id)?;
                Ok(member)
            })
            .await?;
        self.hub.links.stop(&member.id);
        Ok(member)
    }

    /// A pairing code, from the hub itself or through this machine's link.
    pub(crate) async fn hub_pairing(&self) -> Result<Pairing> {
        if self.is_hub() {
            return self.issue_pairing().await;
        }
        match self.through_hub(Call::Pair).await? {
            Reply::Pairing(pairing) => Ok(pairing),
            reply => Err(unexpected(reply)),
        }
    }

    /// Revoke the member `selector` names, on the hub itself or through
    /// this machine's link.
    pub(crate) async fn revoke_hub_member(&self, selector: String) -> Result<Member> {
        if self.is_hub() {
            return self.revoke_member(&selector).await;
        }
        match self.through_hub(Call::Revoke { member: selector }).await? {
            Reply::Revoked(member) => Ok(member),
            reply => Err(unexpected(reply)),
        }
    }
}

/// The overview a [`Call::Members`] reply carries.
pub(crate) fn overview(reply: Reply) -> Result<Overview> {
    match reply {
        Reply::Members(overview) => Ok(overview),
        reply => Err(unexpected(reply)),
    }
}

fn unexpected(reply: Reply) -> anyhow::Error {
    anyhow!("the hub answered with an unexpected {reply:?}")
}
