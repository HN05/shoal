//! The hub role this machine takes or the hub it joined, its settings,
//! and its own identity and member record as the hub.
use std::sync::Arc;

use anyhow::Result;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::{HubRole, Machine, Overview};

use crate::{Hub, answers, daemon::Daemon, identity, members};

impl Hub {
    /// `[machine.hub]`, none in a daemon that leaves the user's config to
    /// the installed Shoal, which takes the hub role there.
    pub(crate) fn hub_settings(
        &self,
        daemon: &dyn Daemon,
    ) -> Option<shoal_core::config::machine::Hub> {
        (daemon.acts_on_global_config()).then(|| self.config.current().machine.hub.clone())
    }

    /// Whether this daemon takes the hub role.
    pub fn is_hub(&self, daemon: &dyn Daemon) -> bool {
        self.hub_settings(daemon).is_some_and(|hub| hub.enabled)
    }

    /// Whether this daemon reaches other machines now: as the hub, or
    /// through its open link to one.
    pub fn reaches_other_machines(&self, daemon: &dyn Daemon) -> bool {
        self.is_hub(daemon) || self.member.calls().is_some()
    }

    /// This machine as the hub names it: the hub's own, or the member it
    /// joined as; none on a machine in no hub.
    pub async fn this_machine(&self, daemon: &dyn Daemon) -> Result<Option<Machine>> {
        if self.is_hub(daemon) {
            let own = self.own_machine(daemon).await?;
            let (id, name) = (own.id, own.name);
            return Ok(Some(Machine { id, name }));
        }
        let joined = self.store.read(crate::membership::load).await?;
        Ok(joined.map(|joined| Machine {
            id: joined.membership.member,
            name: joined.membership.name,
        }))
    }

    /// The hub role this daemon takes, for `shoal status`.
    pub async fn hub_role(&self, daemon: &dyn Daemon) -> Result<Option<HubRole>> {
        if self.is_hub(daemon) {
            return Ok(Some(HubRole::Hub {
                listener: self.hub_listener(daemon),
                availability: self.own_availability().await?,
            }));
        }
        self.membership_role().await
    }

    /// The hub's listener and members, from the hub itself or through this
    /// machine's link to it.
    pub async fn hub_overview(&self, daemon: &dyn Daemon) -> Result<Overview> {
        if self.is_hub(daemon) {
            return self.local_hub_overview(daemon).await;
        }
        self.through_hub(shoal_protocol::hub::link::Call::Members)
            .await
            .and_then(answers::overview)
    }

    /// The name this machine goes by in its hub, else its host name.
    pub async fn machine_name(&self, daemon: &dyn Daemon) -> String {
        let named = match self.hub_role(daemon).await {
            Ok(Some(HubRole::Hub { .. })) => {
                self.own_machine(daemon).await.ok().map(|own| own.name)
            }
            Ok(Some(HubRole::Member { membership, .. })) => Some(membership.name),
            _ => None,
        };
        named.unwrap_or_else(crate::process::machine::hostname)
    }

    /// The hub's own machine, recording it the first time.
    pub(crate) async fn own_machine(
        &self,
        daemon: &dyn Daemon,
    ) -> Result<shoal_protocol::hub::Member> {
        let own = self.own_member().await?;
        let members = self.local_hub_overview(daemon).await?.members;
        (members.into_iter())
            .find(|member| member.id == own)
            .ok_or_else(|| anyhow::anyhow!("the hub's own machine is not recorded"))
    }

    /// The ID of the hub's own machine, recording it the first time.
    pub(crate) async fn own_member(&self) -> Result<MemberId> {
        let own = self.hub_identity().await?.public.clone();
        let name = crate::process::machine::hostname();
        self.store
            .write(move |tx| {
                members::record_own(tx, &own, &name)?;
                members::list(tx, &own)?
                    .into_iter()
                    .find(|member| member.hub)
                    .map(|member| member.id)
                    .ok_or_else(|| anyhow::anyhow!("the hub's own machine is not recorded"))
            })
            .await
    }
}

impl Hub {
    /// The hub's own key pair, made and kept the first time it is asked
    /// for.
    pub(crate) async fn hub_identity(&self) -> Result<Arc<identity::Identity>> {
        let identity = &self.identity;
        let identity = identity.get_or_try_init(|| async {
            let key = self.store.write(identity::stored_or_new_key).await?;
            identity::Identity::new(&key).map(Arc::new)
        });
        identity.await.cloned()
    }
}
