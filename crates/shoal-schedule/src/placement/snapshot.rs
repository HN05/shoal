//! What a placement policy reads: the start to place and each connected
//! machine as the hub last heard of it.
use serde::{Deserialize, Serialize};
use shoal_ids::member_ids::MemberId;

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
    /// The machine the start was made on.
    pub caller: MemberId,
    /// The machine of the space it goes below, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<MemberId>,
}

/// A connected machine as the hub last heard of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    pub id: MemberId,
    pub name: String,
    /// Whether it stays reachable, so lasting work may count on it.
    pub always_on: bool,
    /// The agent tools it has installed.
    pub agents: Vec<String>,
    /// What it last reported of its repositories and queue; none before
    /// its first report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<Capacity>,
}

/// What a machine reports of its repositories and its queue's room.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capacity {
    /// The origins of its registered repositories, by the key machines
    /// match repositories by.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<String>,
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
}

impl Machine {
    /// Whether it has registered the repository of `origin`; every machine
    /// has what a start without one needs.
    pub fn has_repository(&self, origin: Option<&str>) -> bool {
        let Some(origin) = origin else {
            return true;
        };
        (self.capacity.iter())
            .flat_map(|capacity| &capacity.repositories)
            .any(|registered| registered == origin)
    }

    /// Whether it has installed the agent tool `agent`.
    pub fn has_agent(&self, agent: &str) -> bool {
        self.agents.iter().any(|installed| installed == agent)
    }
}
