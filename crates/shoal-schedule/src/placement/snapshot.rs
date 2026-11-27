//! What a placement policy reads, as the hub builds it.
use std::time::Duration;

use serde::{Deserialize, Serialize};
use shoal_ids::member_ids::MemberId;
use shoal_states::states;
use shoal_time::UnixTime;

use super::{SkippedBy, speeds::Speed};

/// The start to place and the machines connected to the hub, the caller's
/// among them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub request: Request,
    pub machines: Vec<Machine>,
}

/// A start that names no machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// The origin of the repository it starts in, by the key machines
    /// match repositories by, which the caller's machine has registered;
    /// none for a chat without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// The ref a new branch starts from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The agent tool it starts.
    pub agent: String,
    /// The mode its agent starts in.
    pub mode: String,
    /// Whether that mode is `heavy`, holding a heavy slot.
    pub heavy: bool,
    /// How long its work lasts, which decides the machines it may go to.
    pub lifetime: Lifetime,
    /// The machine the start was made on.
    pub caller: MemberId,
    /// The machine of the space it goes below, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<MemberId>,
}

states! {
    /// How long a start's work lasts.
    Lifetime {
        /// Work its user comes back to, such as an agent's workspace, which
        /// needs a machine that stays reachable.
        Lasting => "lasting",
        /// Work nothing keeps once it ends, such as a `--disposable`
        /// workspace, which any machine may run while it is connected.
        Disposable => "disposable",
    }
}

/// A connected machine as the hub last heard of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    pub id: MemberId,
    pub name: String,
    /// Whether it stays reachable, so lasting work may count on it.
    pub always_on: bool,
    /// What it last reported of its repositories, agents and queue; none
    /// before its first report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<Capacity>,
    /// Its speed at the start's repository's work; none where it shares no
    /// run with another machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<Speed>,
}

/// What a machine reports of its repositories, the agents it can launch
/// and its queue's room.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capacity {
    /// Its registered repositories, each with the agents it can launch
    /// there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<Registered>,
    /// The agents it can launch outside a repository, under its global and
    /// shared config.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    /// Its `max_heavy_agents` when fixed, `0` for a machine that runs no
    /// heavy agents; none under dynamic admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_heavy_agents: Option<u32>,
    /// The heavy agents its admission would start now: the slots left
    /// under a fixed limit, under dynamic admission one while measured
    /// memory fits another and none otherwise.
    pub heavy_free: u32,
    /// Whether its admission would start a light agent now, which only a
    /// critical or missing reading or low disk holds back.
    pub light_free: bool,
    /// The bytes of memory new work may use before its warning threshold,
    /// beside what its running work is expected to use; none while it
    /// reads none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_free: Option<u64>,
    /// The heavy agents waiting in its queue.
    pub heavy_waiting: u32,
    /// Whether its user walked away, so it admits nothing new.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub away: bool,
    /// Since when its user paused it, so it admits nothing new until they
    /// unpause it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<UnixTime>,
}

/// A repository a machine registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registered {
    /// Its origin, by the key machines match repositories by.
    pub origin: String,
    /// The agents the machine can launch under the repository's settings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
}

/// How far back the demand a machine reports reaches.
pub const DEMAND_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);

impl Request {
    /// Why the start cannot go to `machine`, none when it can. The caller's
    /// machine needs no registered repository: the start found it there.
    pub fn unfit(&self, machine: &Machine) -> Option<SkippedBy> {
        let caller = machine.id == self.caller;
        let Some(capacity) = machine.capacity.as_ref() else {
            return Some(SkippedBy::Undescribed);
        };
        if capacity.paused.is_some() {
            Some(SkippedBy::Paused)
        } else if capacity.away {
            Some(SkippedBy::Away)
        } else if !caller && !machine.has_repository(self.origin.as_deref()) {
            Some(SkippedBy::NoRepository)
        } else if !machine.launches(self.origin.as_deref(), &self.agent) {
            Some(SkippedBy::NoAgent)
        } else {
            None
        }
    }
}

impl Machine {
    pub fn has_repository(&self, origin: Option<&str>) -> bool {
        let registered = |origin| (self.capacity.iter()).any(|c| c.registered(origin).is_some());
        origin.is_none_or(registered)
    }

    /// Whether it can launch `agent` in the repository of `origin`, or
    /// outside one. A repository it has not reported, as the caller's may
    /// be, goes by the agents it launches outside one.
    pub fn launches(&self, origin: Option<&str>, agent: &str) -> bool {
        let Some(capacity) = &self.capacity else {
            return false;
        };
        let agents = capacity.agents_in(origin).unwrap_or(&capacity.agents);
        agents.iter().any(|launched| launched == agent)
    }
}

impl Capacity {
    /// The agents it launches in the repository of `origin`, or outside a
    /// repository without one; none for a repository it has not reported.
    pub fn agents_in(&self, origin: Option<&str>) -> Option<&[String]> {
        match origin {
            Some(origin) => (self.registered(origin)).map(|registered| &registered.agents[..]),
            None => Some(&self.agents),
        }
    }

    fn registered(&self, origin: &str) -> Option<&Registered> {
        (self.repositories.iter()).find(|registered| registered.origin == origin)
    }
}
