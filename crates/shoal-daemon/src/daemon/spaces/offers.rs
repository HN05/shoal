//! How the request a picker offers spaces for judges each of them: this
//! machine's spaces by the check the request declares, and the other joined
//! machines' by each machine's own daemon.
use std::collections::HashMap;

use anyhow::{Result, bail};
use futures_util::future::join_all;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{Member, directory::HubView},
    model::{Offer, Space, SpaceOffer},
    protocol::{Body, Operation, SpaceMethod},
};

use crate::daemon::{
    log,
    manager::Manager,
    scope::Caller,
    target_checks::{Checked, TargetCheck, Verdict},
};

impl Manager {
    /// How `acting` judges each space of this machine `caller` sees.
    pub(crate) async fn offers_here(
        &self,
        caller: Option<&Caller>,
        acting: &Operation,
    ) -> Result<Vec<SpaceOffer>> {
        Ok(judged(acting, &self.listed_spaces(caller).await?))
    }

    /// How `acting` judges each space of the other joined machines in
    /// `view`, as the daemon of each connected machine judges its own. A
    /// machine that is offline or cannot be asked judges none of its
    /// spaces.
    pub(crate) async fn offers_elsewhere(
        &self,
        caller: Option<&Caller>,
        acting: &Operation,
        view: &HubView,
    ) -> HashMap<SpaceId, Offer> {
        let machines = (view.directory.machines.iter()).filter(|machine| {
            machine.connected
                && machine.id != view.machine
                && (view.directory.spaces.iter()).any(|listed| listed.machine == machine.id)
        });
        let asked = machines.map(|machine| self.judged_by(machine, acting, caller));
        let mut offers = HashMap::new();
        for answer in join_all(asked).await {
            match answer {
                Ok(judged) => offers.extend(judged.into_iter().map(|o| (o.space, o.offer))),
                Err(error) => log!("a machine's spaces stay unjudged for a picker: {error:#}"),
            }
        }
        offers
    }

    /// How `machine`'s daemon judges its own spaces for `acting`, from
    /// `caller`.
    async fn judged_by(
        &self,
        machine: &Member,
        acting: &Operation,
        caller: Option<&Caller>,
    ) -> Result<Vec<SpaceOffer>> {
        let acting = Box::new(serde_json::from_value(serde_json::to_value(acting)?)?);
        let judging = Operation::Space(SpaceMethod::JudgeSpaces { acting });
        match self.hub.ask_machine(self, machine, judging, caller).await? {
            Body::SpaceOffers(offers) => Ok(offers),
            other => bail!(
                "machine {} judged spaces with {}",
                machine.name,
                other.variant_name()
            ),
        }
    }
}

/// How `acting` judges each of `spaces`, by the check it declares.
pub(crate) fn judged(acting: &Operation, spaces: &[Space]) -> Vec<SpaceOffer> {
    let check = acting.target_check();
    (spaces.iter())
        .map(|space| SpaceOffer {
            space: space.id.clone(),
            offer: offer(check, space),
        })
        .collect()
}

/// How a request with `check` judges `space`; a request that names no
/// space takes any.
fn offer(check: Option<TargetCheck>, space: &Space) -> Offer {
    match check.map(|check| check.judge(space)) {
        None | Some(Verdict::Accepted) => Offer::Accepted,
        Some(Verdict::Refused(error)) => Offer::Refused {
            reason: format!("{error:#}"),
        },
        Some(Verdict::Lacking(_)) => Offer::Lacking,
    }
}

#[cfg(test)]
mod tests;
