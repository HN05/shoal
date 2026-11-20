//! Which machine owns a space ID, as the hub's directory says: a restore
//! asks before it opens a space again, so the same space never runs on two
//! machines, an import claims the space it opens, with its number, and the
//! source of a move through the hub settles one whose arrival it could not
//! learn.
use anyhow::{Result, anyhow, bail};
use shoal_ids::{
    member_ids::MemberId,
    move_tickets::MoveTicket,
    space_ids::{SpaceId, SpaceNumber},
};
use shoal_protocol::hub::{
    Member,
    link::{Call, Reply},
};

use super::membership;
use crate::{Hub, daemon::Daemon, log};

impl Hub {
    /// Refuse to open the space `id` again while the hub lists it on
    /// another machine. A hub that cannot be asked lets it, and a space
    /// then on two machines shows as a conflict once both report it.
    pub async fn refuse_live_elsewhere(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<()> {
        let owner = match self.owner_elsewhere(daemon, id).await {
            Ok(owner) => owner,
            Err(error) => {
                log!("hub: could not ask which machine has space {id}: {error:#}");
                None
            }
        };
        let Some(owner) = owner else {
            return Ok(());
        };
        let name = &owner.name;
        bail!(
            "space {id} runs on machine {name}; to have it here, run shoal move {id} --ssh with this machine's host on {name}, or keep it there and copy it as a new space with shoal start --copy {id}"
        )
    }

    /// Claim the space `id` at the hub, which this machine imports as
    /// another moves it here, so this machine's report of it takes the ID
    /// over without a conflict, and keep the number the hub keeps for it
    /// for when the import records it. A hub that cannot be asked takes no
    /// claim, and the space shows as a conflict until the other machine
    /// ends it.
    pub async fn claim_space(&self, daemon: &dyn Daemon, id: &SpaceId) {
        if let Err(error) = self.claim_if_joined(daemon, id).await {
            log!("hub: could not claim space {id}: {error:#}");
        }
    }

    /// Claim the space `id` at the hub as [`Self::claim_space`] does, on
    /// the hub or a machine that joined one.
    async fn claim_if_joined(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<()> {
        if !self.is_hub(daemon) && self.store.read(membership::load).await?.is_none() {
            return Ok(());
        }
        self.claim_with(daemon, id, None).await
    }

    /// Claim the space `id` at the hub for the move `ticket` through it,
    /// which this machine imports, and keep the number the hub keeps for
    /// it; refused while the hub cannot be asked, or once the move's
    /// source settled it without this claim, so the import records nothing
    /// and the space stays where it was.
    pub async fn claim_moved(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        ticket: &MoveTicket,
    ) -> Result<()> {
        self.claim_with(daemon, id, Some(ticket)).await
    }

    /// Claim the space `id` at the hub, with the `ticket` of the move that
    /// brings it, and keep the number the hub keeps for it, if any.
    async fn claim_with(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        ticket: Option<&MoveTicket>,
    ) -> Result<()> {
        if let Some(number) = self.claim_at_hub(daemon, id, ticket).await? {
            self.keep_imported_number(id, number).await?;
        }
        Ok(())
    }

    /// Claim the space `id` at the hub, with `ticket`, and the number the
    /// hub keeps for it, if any.
    async fn claim_at_hub(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        ticket: Option<&MoveTicket>,
    ) -> Result<Option<SpaceNumber>> {
        if self.is_hub(daemon) {
            let own = self.own_member().await?;
            return self.record_space_claim(&own, id, ticket).await;
        }
        let call = Call::Claim {
            space: id.clone(),
            ticket: ticket.cloned(),
        };
        match self.through_hub(call).await? {
            Reply::Claimed(number) => Ok(number),
            reply => Err(anyhow!("the hub answered with an unexpected {reply:?}")),
        }
    }

    /// Settle this machine's move `ticket` of the space `id` to `machine`,
    /// whose arrival it could not learn from that machine: whether that
    /// machine claimed the space; when it did not, its claims with the
    /// ticket are refused from then on. Fails while the hub cannot be
    /// asked, which leaves the move unsettled.
    pub async fn settle_move(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        machine: &MemberId,
        ticket: &MoveTicket,
    ) -> Result<bool> {
        if self.is_hub(daemon) {
            return self.record_settled_move(id, machine, ticket).await;
        }
        let call = Call::Settle {
            space: id.clone(),
            machine: machine.clone(),
            ticket: ticket.clone(),
        };
        match self.through_hub(call).await? {
            Reply::Settled(claimed) => Ok(claimed),
            reply => Err(anyhow!("the hub answered with an unexpected {reply:?}")),
        }
    }

    /// The other machine the hub lists the space `id` on; none while this
    /// machine claims the space, whose import it retries, and on a machine
    /// that is not the hub and joined none.
    async fn owner_elsewhere(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<Option<Member>> {
        let (this, owner) = if self.is_hub(daemon) {
            let own = self.own_member().await?;
            let owner = self.local_space_owner(daemon, id, &own).await?;
            (own, owner)
        } else {
            let Some(joined) = self.store.read(membership::load).await? else {
                return Ok(None);
            };
            let owner = match self.through_hub(Call::Owner { space: id.clone() }).await? {
                Reply::Owner(owner) => owner,
                reply => return Err(anyhow!("the hub answered with an unexpected {reply:?}")),
            };
            (joined.membership.member, owner)
        };
        Ok(owner.filter(|owner| owner.id != this))
    }
}
