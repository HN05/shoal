//! What the hub answers, to its members' calls and to the requests of its
//! own machine's callers alike: pairing codes, its listener and members,
//! revoking credentials, its directory of the machines' spaces, and the
//! requests and events one machine sends another.
use anyhow::{Result, anyhow};
use shoal_protocol::hub::{
    Member, MemberKind, Overview,
    link::{Call, Reply},
    pairing::Pairing,
};

use std::sync::Arc;

use shoal_ids::member_ids::MemberId;
use tokio::sync::OwnedRwLockReadGuard;

use super::{directory, members};
use crate::{
    daemon::workspace::Manager,
    refusal::{Reason, Refusal},
};

/// What an answer holds until its reply is written: the daemon operation,
/// so an update never exits between a change and the reply that reports
/// it, and for a member whose credential the call revoked, by leaving or
/// revoking itself, the end of its link once it has read that it did.
pub(super) struct Held {
    _operation: Option<OwnedRwLockReadGuard<()>>,
    ends: Option<(Arc<Manager>, MemberId)>,
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some((manager, member)) = self.ends.take() {
            manager.hub.links.stop(&member);
        }
    }
}

impl Manager {
    /// The hub's reply to `member`'s `call`, with what it holds until the
    /// reply is written.
    pub(super) async fn answer_member(
        self: &Arc<Self>,
        member: &Member,
        call: Call,
    ) -> (Reply, Held) {
        let Some(operation) = self.gates.operation_unless_exiting().await else {
            let message = "the hub's daemon is restarting; try again once it is back".into();
            let held = Held {
                _operation: None,
                ends: None,
            };
            return (Reply::Failed { message }, held);
        };
        let reply = super::session::reply(self.answer_call(member, call).await);
        let revoked_itself = match &reply {
            Reply::Left => true,
            Reply::Revoked(revoked) => revoked.id == member.id,
            _ => false,
        };
        let held = Held {
            _operation: Some(operation),
            ends: revoked_itself.then(|| (self.clone(), member.id.clone())),
        };
        (reply, held)
    }

    /// The hub's reply to `member`'s `call`, refused once the hub revoked
    /// the member, whose link may still be closing.
    async fn answer_call(&self, member: &Member, call: Call) -> Result<Reply> {
        self.ensure_hub()?;
        self.ensure_not_revoked(&member.id).await?;
        Ok(match call {
            Call::Pair => Reply::Pairing(self.issue_pairing().await?),
            Call::Members => Reply::Members(self.local_hub_overview().await?),
            Call::Revoke { member: selector } => {
                Reply::Revoked(self.revoke_member(&selector, Some(&member.id)).await?)
            }
            Call::Leave => {
                self.release_member(&member.id).await?;
                Reply::Left
            }
            Call::Report { report, reserved } => {
                ensure_machine(member)?;
                Reply::Reported(self.record_report(&member.id, report, reserved).await?)
            }
            Call::Claim { space } => {
                ensure_machine(member)?;
                Reply::Claimed(self.record_space_claim(&member.id, &space).await?)
            }
            Call::Directory => Reply::Directory(self.local_directory().await?),
            Call::Owner { space } => {
                Reply::Owner(self.local_space_owner(&space, &member.id).await?)
            }
            Call::Forward {
                machine,
                request,
                delivery,
            } => (self.route_request(member, &machine, request, delivery)).await?,
            Call::Tell { told } => {
                ensure_machine(member)?;
                self.route_told(&member.id, told).await?
            }
            Call::Serve { .. } | Call::Expired { .. } => {
                anyhow::bail!("only the hub passes requests and expiries to machines")
            }
        })
    }

    /// Refuse a member's call once this daemon no longer takes the hub
    /// role, whose links are then closing.
    fn ensure_hub(&self) -> Result<()> {
        anyhow::ensure!(self.is_hub(), "this daemon no longer takes the hub role");
        Ok(())
    }

    /// Refuse a call of the member `id` once the hub revoked it.
    async fn ensure_not_revoked(&self, id: &MemberId) -> Result<()> {
        let own = self.hub_identity().await?.public.clone();
        let id = id.clone();
        let recorded = self.store.read(move |tx| members::find(tx, &id, &own));
        match recorded.await? {
            Some(recorded) if !recorded.revoked => Ok(()),
            _ => Err(anyhow!("the hub revoked this machine's credential")),
        }
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
    /// link, unless it is the `caller`'s, which ends once the caller has
    /// read the reply.
    pub(super) async fn revoke_member(
        &self,
        selector: &str,
        caller: Option<&MemberId>,
    ) -> Result<Member> {
        let own = self.hub_identity().await?.public.clone();
        let selector = selector.to_owned();
        let _admission = self.hub.admissions.lock().await;
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
                directory::forget(tx, &member.id)?;
                super::queue::forget(tx, &member.id)?;
                Ok(member)
            })
            .await?;
        if caller != Some(&member.id) {
            self.hub.links.stop(&member.id);
        }
        Ok(member)
    }

    /// Revoke the credential of `member`, which leaves: its link stays
    /// open for the reply, and the member then closes it.
    async fn release_member(&self, member: &MemberId) -> Result<()> {
        let member = member.clone();
        let _admission = self.hub.admissions.lock().await;
        self.store
            .write(move |tx| {
                members::revoke(tx, &member)?;
                directory::forget(tx, &member)?;
                super::queue::forget(tx, &member)
            })
            .await
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
            return self.revoke_member(&selector, None).await;
        }
        match self.through_hub(Call::Revoke { member: selector }).await? {
            Reply::Revoked(member) => Ok(member),
            reply => Err(unexpected(reply)),
        }
    }
}

/// Refuse a call only a machine makes, such as a report of its spaces or
/// an event one passes up, from a device.
fn ensure_machine(member: &Member) -> Result<()> {
    anyhow::ensure!(
        member.kind == MemberKind::Machine,
        "only a machine reports, claims and passes up spaces at the hub"
    );
    Ok(())
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
