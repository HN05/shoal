//! How the request a picker offers spaces for judges each of them: this
//! machine's spaces by the check the request declares, and the other joined
//! machines' by each machine's own daemon.
use crate::Spaces;
use crate::daemon::Daemon;
use std::collections::HashMap;

use anyhow::{Result, bail};
use futures_util::future::join_all;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{Member, directory::HubView},
    model::{Offer, Space, SpaceOffer},
    protocol::{Body, Operation, SpaceMethod},
};

use crate::{
    log,
    scope::Caller,
    target_checks::{Facts, TargetCheck, Verdict},
};

impl Spaces {
    pub async fn offers_here(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        acting: &Operation,
    ) -> Result<Vec<SpaceOffer>> {
        let spaces = self.listed_spaces(daemon, caller).await?;
        Ok(self.judged_here(daemon, caller, acting, &spaces).await)
    }

    /// With the facts its check reads of them.
    pub async fn judged_here(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        acting: &Operation,
        spaces: &[Space],
    ) -> Vec<SpaceOffer> {
        let facts = match daemon.target_check(acting) {
            Some(check) => self.facts(daemon, caller, check, spaces).await,
            None => Facts::default(),
        };
        judged(daemon.target_check(acting), spaces, &facts)
    }

    /// The daemon of each connected machine judges its own. A machine that
    /// is offline or cannot be asked judges none of its spaces.
    pub async fn offers_elsewhere(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        acting: &Operation,
        view: &HubView,
    ) -> HashMap<SpaceId, Offer> {
        let asked =
            (judging_machines(view)).map(|machine| self.judged_by(daemon, machine, acting, caller));
        let mut offers = HashMap::new();
        for answer in join_all(asked).await {
            match answer {
                Ok(judged) => offers.extend(judged.into_iter().map(|o| (o.space, o.offer))),
                Err(error) => log!("a machine's spaces stay unjudged for a picker: {error:#}"),
            }
        }
        offers
    }

    async fn judged_by(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        acting: &Operation,
        caller: Option<&Caller>,
    ) -> Result<Vec<SpaceOffer>> {
        let acting = Box::new(serde_json::from_value(serde_json::to_value(acting)?)?);
        let judging = Operation::Space(SpaceMethod::JudgeSpaces { acting });
        match daemon.ask_machine(machine, judging, caller).await? {
            Body::SpaceOffers(offers) => Ok(offers),
            other => bail!(
                "machine {} judged spaces with {}",
                machine.name,
                other.variant_name()
            ),
        }
    }
}

fn judging_machines(view: &HubView) -> impl Iterator<Item = &Member> {
    (view.directory.machines.iter()).filter(|machine| {
        machine.connected
            && machine.id != view.machine
            && (view.directory.spaces.iter()).any(|listed| listed.machine == machine.id)
    })
}

pub fn judged(check: Option<TargetCheck>, spaces: &[Space], facts: &Facts) -> Vec<SpaceOffer> {
    (spaces.iter())
        .map(|space| SpaceOffer {
            space: space.id.clone(),
            offer: offer(check, space, facts),
        })
        .collect()
}

/// A request that names no space takes any.
fn offer(check: Option<TargetCheck>, space: &Space, facts: &Facts) -> Offer {
    match check.map(|check| check.judge(space, facts)) {
        None | Some(Verdict::Accepted) => Offer::Accepted,
        Some(Verdict::Refused(error)) => Offer::Refused {
            reason: format!("{error:#}"),
        },
        Some(Verdict::Lacking(_)) => Offer::Lacking,
    }
}
