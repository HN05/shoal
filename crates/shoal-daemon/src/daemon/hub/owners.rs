//! Which machine owns a space ID, as the hub's directory says: a restore
//! asks before it opens a space again, so the same space never runs on two
//! machines, and an import claims the space it opens, with its number.
use anyhow::{Result, anyhow, bail};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::hub::{
    Member,
    link::{Call, Reply},
};

use super::membership;
use crate::daemon::{log, manager::Manager};

impl Manager {
    /// Refuse to open the space `id` again while the hub lists it on
    /// another machine. A hub that cannot be asked lets it, and a space
    /// then on two machines shows as a conflict once both report it.
    pub(crate) async fn refuse_live_elsewhere(&self, id: &SpaceId) -> Result<()> {
        let owner = match self.owner_elsewhere(id).await {
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
            "space {id} runs on machine {name}; to have it here, run shoal move ssh with this machine's host and {id} on {name}, or keep it there and copy it as a new space with shoal start --copy {id}"
        )
    }

    /// Claim the space `id` at the hub, which this machine imports as
    /// another moves it here, so this machine's report of it takes the ID
    /// over without a conflict, and keep the number the hub keeps for it
    /// for when the import records it. A hub that cannot be asked takes no
    /// claim, and the space shows as a conflict until the other machine
    /// ends it.
    pub(crate) async fn claim_space(&self, id: &SpaceId) {
        let claimed = async {
            if let Some(number) = self.claim_at_hub(id).await? {
                self.keep_imported_number(id, number).await?;
            }
            anyhow::Ok(())
        };
        if let Err(error) = claimed.await {
            log!("hub: could not claim space {id}: {error:#}");
        }
    }

    /// Claim the space `id` at the hub, and the number the hub keeps for
    /// it, if any.
    async fn claim_at_hub(&self, id: &SpaceId) -> Result<Option<SpaceNumber>> {
        if self.is_hub() {
            return self.record_space_claim(&self.own_member().await?, id).await;
        }
        if self.store.read(membership::load).await?.is_none() {
            return Ok(None);
        }
        match self.through_hub(Call::Claim { space: id.clone() }).await? {
            Reply::Claimed(number) => Ok(number),
            reply => Err(anyhow!("the hub answered with an unexpected {reply:?}")),
        }
    }

    /// The other machine the hub lists the space `id` on; none while this
    /// machine claims the space, whose import it retries, and on a machine
    /// that is not the hub and joined none.
    async fn owner_elsewhere(&self, id: &SpaceId) -> Result<Option<Member>> {
        let (this, owner) = if self.is_hub() {
            let own = self.own_member().await?;
            let owner = self.local_space_owner(id, &own).await?;
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
