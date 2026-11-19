//! Each machine's availability, which the hub works out: a machine
//! describes itself when its link opens and when its setting changes. `[machine] availability` sets it; under `auto` a machine the
//! hub has seen there for less than [`NEW_FOR`] is intermittent when it has
//! a battery, so a new portable machine takes no lasting work before the
//! hub has seen it behave, and any other machine is intermittent when it
//! was there less than [`ALWAYS_ON_PERCENT`] of the last week.
use std::{collections::HashMap, convert::Infallible, time::Duration};

use anyhow::{Result, anyhow};
use rusqlite::Transaction;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::{
    MemberKind,
    availability::{
        Availability, AvailabilityReason, AvailabilitySetting, Classified, Description,
    },
    link::{Call, Reply},
};

use super::{
    members,
    presence::{self, History, KEPT, SHORT_GAP},
    session::Calls,
};
use crate::{
    daemon::{battery, log, manager::Manager},
    time::UnixTime,
};

/// How long the hub goes by a machine's battery before its history.
const NEW_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// The share of the last week, in percent, a machine that is always on was
/// there at least.
const ALWAYS_ON_PERCENT: u64 = 95;

/// How often the hub marks the machines that are there.
const MARK_EVERY: Duration = Duration::from_secs(60);

/// How long a machine waits to describe itself again after it failed to.
const RETRY: Duration = Duration::from_secs(30);

/// The availability at `now` of a machine that has a `battery`, none while
/// it never said, and is set to `setting`, whose presence the hub recorded
/// as `history`.
pub(super) fn classify(
    battery: Option<bool>,
    setting: AvailabilitySetting,
    history: &History,
    now: UnixTime,
) -> Classified {
    if let Some(availability) = setting.set() {
        return Classified {
            availability,
            reason: AvailabilityReason::Set,
        };
    }
    match history.since {
        Some(since) if now.duration_since(since) >= NEW_FOR => {
            by_history(&history.spans, since.max(now - KEPT), now)
        }
        _ => new_machine(battery),
    }
}

/// The availability of a machine the hub has seen for less than
/// [`NEW_FOR`], which has a `battery`, none while it never said.
fn new_machine(battery: Option<bool>) -> Classified {
    let availability = match battery {
        Some(false) => Availability::AlwaysOn,
        Some(true) | None => Availability::Intermittent,
    };
    Classified {
        availability,
        reason: AvailabilityReason::New { battery },
    }
}

/// The availability of a machine that was there through `spans` over
/// `window` to `now`.
fn by_history(spans: &[presence::Span], window: UnixTime, now: UnixTime) -> Classified {
    let there = there_for(spans, window, now).as_secs();
    let length = now.duration_since(window).as_secs().max(1);
    let availability = match there * 100 >= length * ALWAYS_ON_PERCENT {
        true => Availability::AlwaysOn,
        false => Availability::Intermittent,
    };
    let percent = (there * 100 / length).min(100) as u8;
    Classified {
        availability,
        reason: AvailabilityReason::Connected { percent },
    }
}

/// How long of `window` to `now` the machine was there through `spans`,
/// oldest first, counting an absence since the last up to `now` as there
/// while it is shorter than [`SHORT_GAP`].
fn there_for(spans: &[presence::Span], window: UnixTime, now: UnixTime) -> Duration {
    let last = spans.len().checked_sub(1);
    (spans.iter().enumerate())
        .map(|(index, span)| {
            let ongoing = Some(index) == last && now.duration_since(span.end) < SHORT_GAP;
            let end = match ongoing {
                true => now,
                false => span.end.min(now),
            };
            end.duration_since(span.start.max(window))
        })
        .sum()
}

/// The availability of the machine `member` at `now`.
fn of_member(tx: &Transaction<'_>, member: &MemberId, now: UnixTime) -> Result<Classified> {
    let (battery, setting) = members::description(tx, member)?;
    let history = presence::history(tx, member, now)?;
    Ok(classify(battery, setting, &history, now))
}

/// The availability of each of `machines` at `now`.
pub(super) fn of_machines(
    tx: &Transaction<'_>,
    machines: impl IntoIterator<Item = MemberId>,
    now: UnixTime,
) -> Result<HashMap<MemberId, Classified>> {
    (machines.into_iter())
        .map(|member| Ok((member.clone(), of_member(tx, &member, now)?)))
        .collect()
}

impl Manager {
    /// Mark the hub's own machine there, with the machines whose links are
    /// open, every [`MARK_EVERY`] while this daemon is the hub. A reload
    /// marks at once, so a changed `[machine] availability` applies.
    pub(super) async fn keep_marking_presence(&self) {
        let mut every = tokio::time::interval(MARK_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut reloaded = self.config_state.watch();
        loop {
            tokio::select! {
                _ = every.tick() => {}
                changed = reloaded.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
            if !self.is_hub() {
                continue;
            }
            if let Err(error) = self.mark_presence().await {
                log!("hub: could not record which machines are there: {error:#}");
            }
        }
    }

    /// Mark the hub's own machine, as it describes itself now, and the
    /// machines whose links are open, there now, forgetting spans past
    /// what the hub keeps.
    async fn mark_presence(&self) -> Result<()> {
        let own = self.own_member().await?;
        let description = Description {
            battery: battery::present(),
            availability: self.config().machine.availability,
        };
        let connected = self.hub.links.connected();
        self.store
            .write(move |tx| {
                let now = UnixTime::now();
                members::describe(tx, &own, description)?;
                for member in std::iter::once(&own).chain(&connected) {
                    presence::mark(tx, member, now)?;
                }
                presence::prune(tx, now)
            })
            .await
    }

    /// Record what the machine `member` says of itself, and the
    /// availability it then has.
    pub(super) async fn describe_member(
        &self,
        member: &MemberId,
        description: Description,
    ) -> Result<Classified> {
        let id = member.clone();
        self.store
            .write(move |tx| {
                members::describe(tx, &id, description)?;
                of_member(tx, &id, UnixTime::now())
            })
            .await
    }

    /// Give each machine among `members` its availability.
    pub(super) async fn with_availability(
        &self,
        members: &mut [shoal_protocol::hub::Member],
    ) -> Result<()> {
        let machines: Vec<MemberId> = (members.iter())
            .filter(|member| member.kind == MemberKind::Machine)
            .map(|member| member.id.clone())
            .collect();
        let mut classified = (self.store)
            .read(move |tx| of_machines(tx, machines, UnixTime::now()))
            .await?;
        for member in members {
            member.availability = classified.remove(&member.id);
        }
        Ok(())
    }
}

impl Manager {
    /// Describe this machine to the hub over its link, and again whenever
    /// `[machine] availability` changes, for as long as the link stays
    /// open.
    pub(super) async fn keep_describing(&self, calls: &Calls) -> Infallible {
        let mut reloaded = self.config_state.watch();
        loop {
            let setting = self.config().machine.availability;
            if let Err(error) = self.describe(calls, setting).await {
                log!("hub: could not describe this machine to the hub: {error:#}");
                tokio::time::sleep(RETRY).await;
                continue;
            }
            while self.config().machine.availability == setting {
                if reloaded.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        }
    }

    /// Describe this machine, set to `setting`, to the hub.
    async fn describe(&self, calls: &Calls, setting: AvailabilitySetting) -> Result<()> {
        let description = Description {
            battery: battery::present(),
            availability: setting,
        };
        match calls.call(Call::Describe { description }).await? {
            Reply::Availability(_) => Ok(()),
            reply => Err(anyhow!("the hub answered with an unexpected {reply:?}")),
        }
    }
}

#[cfg(test)]
mod tests;
