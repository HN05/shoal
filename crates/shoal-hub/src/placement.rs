//! Where a new space starts among the hub's machines: the hub ranks its
//! connected machines by the policy `[machine.hub] placement` names, from
//! a snapshot of the start and each machine as it last described itself
//! and reported its capacity, which the hub keeps in memory for as long as
//! it runs, or asks the app registered as the placer. A machine that
//! joined the hub asks it over its link.
use std::{collections::HashMap, sync::Mutex};

use anyhow::{Result, anyhow};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::{
    Member, MemberKind,
    availability::Availability,
    link::{Call, Reply},
    placement::{Decision, Skipped, Unanswered},
};
use shoal_schedule::placement::{
    Balanced, Local, Placement, Placer, Policy,
    snapshot::{Capacity, Machine, Request, Snapshot},
};

use crate::{
    Hub,
    daemon::Daemon,
    log,
    refusal::{Reason, Refusal},
};

mod placer;

pub(crate) use placer::Placers;
pub use placer::Registration;

/// The capacity each machine last reported.
#[derive(Default)]
pub(crate) struct Capacities(Mutex<HashMap<MemberId, Capacity>>);

impl Capacities {
    /// Keep `capacity` as what the machine `member` reported last.
    pub fn keep(&self, member: &MemberId, capacity: Capacity) {
        let mut capacities = self.0.lock().unwrap();
        capacities.insert(member.clone(), capacity);
    }

    /// What the machine `member` reported last, none before it reported.
    pub fn of(&self, member: &MemberId) -> Option<Capacity> {
        let capacities = self.0.lock().unwrap();
        capacities.get(member).cloned()
    }
}

impl Hub {
    /// Where a start of `request`, made on this machine, goes among the
    /// hub's machines, as the hub decides; none where no machine can take
    /// it. It fails while the hub cannot be asked.
    pub async fn place(&self, daemon: &dyn Daemon, request: Request) -> Result<Option<Decision>> {
        if self.is_hub(daemon) {
            return self.place_for(daemon, request).await;
        }
        match self.through_hub(Call::Place { request }).await? {
            Reply::Placed(decision) => Ok(decision),
            reply => Err(anyhow!("the hub answered with an unexpected {reply:?}")),
        }
    }

    /// As the hub: where a start of `request` goes among the connected
    /// machines, by the configured policy.
    pub(super) async fn place_for(
        &self,
        daemon: &dyn Daemon,
        request: Request,
    ) -> Result<Option<Decision>> {
        let members = self.local_hub_overview(daemon).await?.members;
        let snapshot = Snapshot {
            request,
            machines: placed_machines(&members),
        };
        let (placement, decided) = self.rank(&snapshot).await;
        Ok(decision(placement, decided, &members))
    }

    /// The placement of `snapshot` by the configured policy, with the
    /// policy that made it and why the placer an `external` one asks did
    /// not, where it did not.
    async fn rank(&self, snapshot: &Snapshot) -> (Placement, Decided) {
        match self.config.current().machine.hub.placement {
            Policy::Local => (Local.place(snapshot), (Policy::Local, None)),
            Policy::Balanced => (Balanced.place(snapshot), (Policy::Balanced, None)),
            Policy::External => self.place_externally(snapshot).await,
        }
    }

    /// The registered placer's placement of `snapshot`, else the
    /// `balanced` one.
    async fn place_externally(&self, snapshot: &Snapshot) -> (Placement, Decided) {
        let placed = self.placers.ask(snapshot).await;
        match placed.and_then(|placement| among(placement, snapshot)) {
            Ok(placement) => (placement, (Policy::External, None)),
            Err(unanswered) => balanced_instead(snapshot, unanswered),
        }
    }

    /// Register the caller as the placer an `external` policy asks, in
    /// place of the one before, for as long as it keeps the registration;
    /// refused off the hub's machine, which asks it.
    pub fn serve_placement(&self, daemon: &dyn Daemon) -> Result<Registration> {
        if !self.is_hub(daemon) {
            let message = "the placer registers on the hub's machine, which asks it";
            return Err(Refusal::new(Reason::NotReady, message).into());
        }
        Ok(self.placers.register())
    }

    /// Take `placement` as the placer's answer to the snapshot `number`;
    /// refused once the hub no longer waits for it.
    pub fn answer_placement(&self, number: u64, placement: Placement) -> Result<()> {
        if !self.placers.answer(number, placement) {
            let message = format!("the hub no longer waits for an answer to placement {number}");
            return Err(Refusal::new(Reason::NotFound, message).into());
        }
        Ok(())
    }

    /// Give each connected machine among `members` the capacity it last
    /// reported.
    pub(super) fn with_capacity(&self, members: &mut [Member]) {
        for member in members.iter_mut().filter(|member| member.connected) {
            member.capacity = self.capacities.of(&member.id);
        }
    }
}

/// The policy that made a placement, and why the placer an `external` one
/// asks did not, where it did not.
type Decided = (Policy, Option<Unanswered>);

/// The `balanced` placement of `snapshot`, which the hub's log records, in
/// place of the external placer's, which left it `unanswered`.
fn balanced_instead(snapshot: &Snapshot, unanswered: Unanswered) -> (Placement, Decided) {
    log!("hub: placed a start by balanced, since the external placer did not: {unanswered}");
    (
        Balanced.place(snapshot),
        (Policy::Balanced, Some(unanswered)),
    )
}

/// `placement`, where it chooses a machine of `snapshot`; else the placer
/// that made it failed.
fn among(placement: Placement, snapshot: &Snapshot) -> Result<Placement, Unanswered> {
    let chosen = placement.chosen().map(|placed| &placed.machine);
    let known = chosen
        .is_some_and(|chosen| (snapshot.machines.iter()).any(|machine| &machine.id == chosen));
    match known {
        true => Ok(placement),
        false => Err(Unanswered::Failed),
    }
}

/// The connected machines among `members`, as placement reads them.
fn placed_machines(members: &[Member]) -> Vec<Machine> {
    (members.iter())
        .filter(|member| member.kind == MemberKind::Machine && member.connected)
        .map(|member| Machine {
            id: member.id.clone(),
            name: member.name.clone(),
            always_on: (member.availability)
                .is_some_and(|classified| classified.availability == Availability::AlwaysOn),
            agents: (member.agents.iter())
                .map(|agent| agent.name().to_owned())
                .collect(),
            capacity: member.capacity.clone(),
        })
        .collect()
}

/// The decision `placement` makes, which the policy `decided` made, after
/// the placer it names left it `unanswered`, naming the machines by their
/// names among `members`; none without a machine to go to.
fn decision(
    placement: Placement,
    (decided, unanswered): Decided,
    members: &[Member],
) -> Option<Decision> {
    let name = |id: &MemberId| {
        let member = members.iter().find(|member| &member.id == id);
        member.map_or_else(|| id.to_string(), |member| member.name.clone())
    };
    let chosen = placement.chosen()?;
    Some(Decision {
        machine: chosen.machine.clone(),
        name: name(&chosen.machine),
        reason: chosen.reason,
        policy: decided,
        unanswered,
        skipped: (placement.skipped.iter())
            .map(|skipped| Skipped {
                name: name(&skipped.machine),
                why: skipped.why,
            })
            .collect(),
    })
}
