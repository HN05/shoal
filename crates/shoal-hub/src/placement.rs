//! Where a new space starts among the hub's machines.
use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
    time::Duration,
};

use anyhow::{Result, anyhow};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::{
    Member, MemberKind,
    availability::Availability,
    directory::Report,
    link::{Call, Reply},
    placement::{Decision, Skipped, Unanswered},
};
use shoal_schedule::placement::{
    Balanced, Local, Placement, Placer, Policy,
    snapshot::{Capacity, Exclusive, Machine, Request, Snapshot},
    speeds::{self, Medians, Speed},
};
use tokio::{sync::watch, time::Instant};

use crate::{
    Hub,
    daemon::Daemon,
    log,
    refusal::{Reason, Refusal},
};

mod placer;

pub(crate) use placer::Placers;
pub use placer::Registration;

/// How long a placement waits for the connected machines that have not
/// reported their capacity since their link opened.
const DESCRIBED_WITHIN: Duration = Duration::from_secs(2);

/// The capacity and the medians of runs each machine last reported, each
/// with the link it reported on, none for the hub's own machine, which has
/// no link.
#[derive(Default)]
pub(crate) struct Capacities {
    reported: Mutex<Reported>,
    /// Counts the reports kept, for those who wait for one.
    kept: watch::Sender<u64>,
}

#[derive(Default)]
struct Reported {
    capacities: OnLinks<Capacity>,
    medians: OnLinks<Medians>,
}

impl Capacities {
    /// Keep `capacity` and `medians` as what the machine `member` reported
    /// with `report` on its link `link`: a whole report, which a machine
    /// sends as its link opens, replaces what the hub had, so one that has
    /// not measured since has none; a report of changes without them keeps
    /// them.
    pub fn keep(
        &self,
        (member, link): (&MemberId, Option<u64>),
        report: &Report,
        capacity: Option<Capacity>,
        medians: Option<Medians>,
    ) {
        let mut reported = self.reported.lock().unwrap();
        self.kept.send_modify(|kept| *kept += 1);
        let whole = matches!(report, Report::Whole(_));
        replace(&mut reported.capacities, (member, link), capacity, whole);
        replace(&mut reported.medians, (member, link), medians, whole);
    }

    /// What the machine `member` reported last on its link `link`, none
    /// before it reported there. So a machine whose link opened again is
    /// undescribed until its first report on it.
    pub fn of(&self, member: &MemberId, link: Option<u64>) -> Option<Capacity> {
        let reported = self.reported.lock().unwrap();
        on_link(&reported.capacities, member, link).cloned()
    }

    /// Changes as each report is kept.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.kept.subscribe()
    }

    /// The speeds per repository of the machines `members` name, from the
    /// medians each reported on its link, compared among them alone.
    fn speeds<'a>(
        &self,
        members: impl Iterator<Item = (&'a MemberId, Option<u64>)>,
    ) -> HashMap<MemberId, BTreeMap<String, Speed>> {
        let reported = self.reported.lock().unwrap();
        let medians: Vec<(&MemberId, &Medians)> = (members)
            .filter_map(|(member, link)| Some((member, on_link(&reported.medians, member, link)?)))
            .collect();
        speeds::speeds(medians)
    }
}

/// What each machine reported, with the link it reported it on.
type OnLinks<T> = HashMap<MemberId, (Option<u64>, T)>;

/// What `member` reported in `kept` on its link `link`, none from another.
fn on_link<'a, T>(kept: &'a OnLinks<T>, member: &MemberId, link: Option<u64>) -> Option<&'a T> {
    let reported = kept.get(member).filter(|(on, _)| *on == link);
    reported.map(|(_, value)| value)
}

/// Keep `value` as what `member` reported in `kept` on its link; without
/// one, a `whole` report forgets what it had.
fn replace<T>(
    kept: &mut OnLinks<T>,
    (member, link): (&MemberId, Option<u64>),
    value: Option<T>,
    whole: bool,
) {
    match (value, whole) {
        (Some(value), _) => {
            kept.insert(member.clone(), (link, value));
        }
        (None, true) => {
            kept.remove(member);
        }
        (None, false) => {}
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
        self.place_among(daemon, request, |_| true).await
    }

    /// As the hub: where a start of `request` goes among the connected
    /// machines that are `eligible`, by the configured policy.
    pub(super) async fn place_among(
        &self,
        daemon: &dyn Daemon,
        request: Request,
        eligible: impl Fn(&Member) -> bool,
    ) -> Result<Option<Decision>> {
        let (snapshot, members) = (self.described_snapshot(daemon, request, eligible)).await?;
        let (placement, decided) = self.rank(&snapshot).await;
        log!(
            "hub: {}",
            logged(&snapshot.request, &placement, decided.0, &members)
        );
        Ok(decision(placement, decided, &members))
    }

    /// The snapshot of `request` among the connected machines that are
    /// `eligible`, with the hub's members, once each of those machines has
    /// reported its capacity, or [`DESCRIBED_WITHIN`] passed.
    async fn described_snapshot(
        &self,
        daemon: &dyn Daemon,
        request: Request,
        eligible: impl Fn(&Member) -> bool,
    ) -> Result<(Snapshot, Vec<Member>)> {
        let mut kept = self.capacities.watch();
        let deadline = Instant::now() + DESCRIBED_WITHIN;
        loop {
            kept.borrow_and_update();
            let members = self.local_hub_overview(daemon).await?.members;
            let snapshot = Snapshot {
                request: request.clone(),
                machines: placed_machines(
                    members.iter().filter(|member| eligible(member)),
                    request.origin.as_deref(),
                ),
            };
            let undescribed = (snapshot.machines.iter()).any(|machine| machine.capacity.is_none());
            if !undescribed || !kept_by(&mut kept, deadline).await {
                return Ok((snapshot, members));
            }
        }
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
        let placed = self.placers.ask(snapshot, placer::WAIT).await;
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
    /// reported, its speeds against the other connected machines, and the
    /// repositories it alone serves by those reports. Read their
    /// availability first.
    pub(super) fn with_capacity(&self, members: &mut [Member]) {
        let connected = (members.iter()).filter(|member| member.connected);
        let links = connected.map(|member| (&member.id, self.links.token(&member.id)));
        let mut speeds = self.capacities.speeds(links);
        for member in members.iter_mut().filter(|member| member.connected) {
            member.capacity = self.capacities.of(&member.id, self.links.token(&member.id));
            member.speeds = speeds.remove(&member.id).unwrap_or_default();
        }
        with_exclusive(members);
    }
}

/// Whether a report is kept by `deadline`, as `kept` watches them.
async fn kept_by(kept: &mut watch::Receiver<u64>, deadline: Instant) -> bool {
    let changed = tokio::time::timeout_at(deadline, kept.changed()).await;
    matches!(changed, Ok(Ok(())))
}

/// Give each machine among `members` with a capacity the repositories it
/// alone serves beside the other `always_on` machines with one.
fn with_exclusive(members: &mut [Member]) {
    let always_on: Vec<(MemberId, Capacity)> = (members.iter())
        .filter(|member| always_on(member))
        .filter_map(|member| Some((member.id.clone(), member.capacity.clone()?)))
        .collect();
    for member in members.iter_mut() {
        let others = (always_on.iter())
            .filter(|(id, _)| *id != member.id)
            .map(|(_, capacity)| capacity);
        member.exclusive =
            (member.capacity.as_ref()).map(|capacity| Exclusive::among(capacity, others));
    }
}

/// Whether `member` is a machine classified `always_on`.
fn always_on(member: &Member) -> bool {
    (member.availability)
        .is_some_and(|classified| classified.availability == Availability::AlwaysOn)
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

/// `placement`, where it chooses a machine of `snapshot` the start can go
/// to, which has its repository and agent tool, since placement never
/// clones a repository; else the placer that made it failed.
fn among(placement: Placement, snapshot: &Snapshot) -> Result<Placement, Unanswered> {
    let chosen = placement.chosen().map(|placed| &placed.machine);
    let fits = (snapshot.machines.iter())
        .find(|machine| Some(&machine.id) == chosen)
        .is_some_and(|machine| snapshot.request.unfit(machine).is_none());
    match fits {
        true => Ok(placement),
        false => Err(Unanswered::Failed),
    }
}

/// The connected machines among `members`, as placement reads them for a
/// start in the repository of `origin`.
fn placed_machines<'a>(
    members: impl Iterator<Item = &'a Member>,
    origin: Option<&str>,
) -> Vec<Machine> {
    members
        .filter(|member| member.kind == MemberKind::Machine && member.connected)
        .map(|member| Machine {
            id: member.id.clone(),
            name: member.name.clone(),
            always_on: always_on(member),
            capacity: member.capacity.clone(),
            speed: origin.and_then(|origin| member.speeds.get(origin).copied()),
            exclusive: member.exclusive.clone().unwrap_or_default(),
        })
        .collect()
}

/// The name of the machine `id` among `members`, its ID where they lack it.
fn machine_name(members: &[Member], id: &MemberId) -> String {
    let member = members.iter().find(|member| &member.id == id);
    member.map_or_else(|| id.to_string(), |member| member.name.clone())
}

/// What the hub's log records of `placement` of a start of `request`, which
/// `policy` made: where it sends the start and why, and why it skips each
/// machine it skips, naming the machines among `members`.
fn logged(request: &Request, placement: &Placement, policy: Policy, members: &[Member]) -> String {
    let name = |id: &MemberId| machine_name(members, id);
    let work = match &request.agent {
        Some(start) => format!("a start of agent {}", start.agent),
        None => "a CI job".to_owned(),
    };
    let start = format!(
        "{work} in {}",
        request.origin.as_deref().unwrap_or("no repository")
    );
    let chosen = match placement.chosen() {
        Some(placed) => format!(
            "goes to machine {} ({:?})",
            name(&placed.machine),
            placed.reason
        ),
        None => "goes to no machine".to_owned(),
    };
    let skipped: Vec<String> = (placement.skipped.iter())
        .map(|skipped| format!("{} ({})", name(&skipped.machine), skipped.why))
        .collect();
    let skipped = match skipped.is_empty() {
        true => "none".to_owned(),
        false => skipped.join(", "),
    };
    format!("{start} {chosen} by {policy}; skipped: {skipped}")
}

/// The decision `placement` makes, which the policy `decided` made, after
/// the placer it names left it `unanswered`, naming the machines by their
/// names among `members`; none without a machine to go to.
fn decision(
    placement: Placement,
    (decided, unanswered): Decided,
    members: &[Member],
) -> Option<Decision> {
    let name = |id: &MemberId| machine_name(members, id);
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

#[cfg(test)]
mod tests;
