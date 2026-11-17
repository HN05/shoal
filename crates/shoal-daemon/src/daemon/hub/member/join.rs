//! Joining the hub, by a pairing code or through a command, and leaving it.
use anyhow::{Result, bail};
use shoal_protocol::{
    hub::{HubUrl, Left, Membership, Route, link::Call},
    protocol::JoinVia,
};

use super::open::{Expected, Opened, Plan, Welcomed};
use crate::{
    daemon::{
        hub::membership::{self, Joined},
        workspace::Manager,
    },
    refusal::{Reason, Refusal},
    time::UnixTime,
};

impl Manager {
    /// Join the hub `via` reaches, as a machine named `name`, or after its
    /// host.
    pub(crate) async fn join_hub(&self, via: JoinVia, name: Option<String>) -> Result<Membership> {
        self.ensure_joinable().await?;
        let (route, expected, code, url) = match via {
            JoinVia::Code(code) => (
                Route::Url(code.url.clone()),
                Expected::Fingerprint(code.key),
                Some(code.code),
                Some(code.url),
            ),
            JoinVia::Command(command) => (Route::Command(command), Expected::Any, None, None),
        };
        let intent = self.join_intent(name, code).await?;
        let plan = Plan {
            route,
            intent,
            expected,
        };
        let welcomed = match self.open_link(plan).await?.1 {
            Opened::Welcomed(welcomed) => welcomed,
            Opened::Refused(refusal) => bail!("the hub refused this machine: {}", refusal.message),
        };
        let membership = self.save_membership(welcomed, url).await?;
        self.hub.member.wake.notify_one();
        Ok(membership)
    }

    /// Refuse a join of the hub, or of a machine that joined it already
    /// and was not revoked.
    async fn ensure_joinable(&self) -> Result<()> {
        if self.is_hub() {
            return Err(Refusal::new(
                Reason::NotReady,
                "this machine is the hub, which joins no other",
            )
            .into());
        }
        if let Some(joined) = self.store.read(membership::load).await?
            && !joined.revoked
        {
            return Err(Refusal::new(
                Reason::NameTaken,
                format!(
                    "this machine joined the hub already, as {} ({}); run shoal hub leave first",
                    joined.membership.name, joined.membership.member
                ),
            )
            .into());
        }
        Ok(())
    }

    /// Keep this machine's membership of the hub that `welcomed` it, which
    /// it paired with at `url`.
    pub(super) async fn save_membership(
        &self,
        welcomed: Welcomed,
        url: Option<HubUrl>,
    ) -> Result<Membership> {
        let Welcomed {
            member,
            name,
            hub_key,
        } = welcomed;
        let joined = Joined {
            membership: Membership {
                member,
                name,
                url,
                joined_at: UnixTime::now(),
            },
            hub_key,
            revoked: false,
        };
        let membership = joined.membership.clone();
        self.store
            .write(move |tx| membership::save(tx, &joined))
            .await?;
        Ok(membership)
    }

    /// Leave the hub: revoke this machine's credential there, when the hub
    /// is reachable, and forget the membership.
    pub(crate) async fn leave_hub(&self) -> Result<Left> {
        let Some(joined) = self.store.read(membership::load).await? else {
            return Err(Refusal::new(Reason::NotFound, "this machine joined no hub").into());
        };
        let revoked = joined.revoked || self.through_hub(Call::Leave).await.is_ok();
        self.store.write(membership::forget).await?;
        self.hub.member.wake.notify_one();
        Ok(Left {
            membership: joined.membership,
            revoked,
        })
    }
}
