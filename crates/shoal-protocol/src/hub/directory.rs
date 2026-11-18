//! The hub's directory of every joined machine's spaces. Each machine owns
//! its spaces' records and reports a summary of each to the hub, which
//! keeps them and serves them as one view; the hub lists each space ID
//! under one machine, the first to report it.
use serde::{Deserialize, Serialize};
use shoal_ids::{
    member_ids::MemberId,
    space_ids::{SpaceId, SpaceNumber},
};

use super::Member;
use crate::{
    approvals::ApprovalRequest, config::modes::ModeName, list_status::ListStatus, model::SpaceKind,
    progress::Progress, questions::Question, space_names::SpaceName, state::AgentState,
    time::UnixTime,
};

/// A space as the machine that owns it reports it to the hub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceSummary {
    pub id: SpaceId,
    /// Its number on its machine.
    pub number: SpaceNumber,
    pub name: SpaceName,
    pub kind: SpaceKind,
    /// Its repository; none for a chat without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<RepositoryOrigin>,
    /// The space above it, which may run on another machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SpaceId>,
    /// The mode its latest agent started in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeName>,
    /// What lists show it doing.
    pub status: ListStatus,
    /// The turn state its running agent last reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<AgentState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// Its open questions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<Question>,
    /// Its pending approval requests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approvals: Vec<ApprovalRequest>,
}

/// A repository as every machine tells it: by its origin, never by the ID
/// each machine gives it, with the name its machine shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryOrigin {
    /// Its origin remote's URL, as `shoal move` matches repositories by;
    /// none for a checkout without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub name: String,
}

/// What a machine reports of its spaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Report {
    /// Every space it has, which replaces what the hub had for it, as a
    /// machine sends once its link opens.
    Whole(Vec<SpaceSummary>),
    /// What changed since its last report.
    Changes {
        changed: Vec<SpaceSummary>,
        removed: Vec<SpaceId>,
    },
}

/// A space in the hub's directory, as its machine last reported it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Listed {
    /// The machine that owns it.
    pub machine: MemberId,
    #[serde(flatten)]
    pub space: SpaceSummary,
    pub reported_at: UnixTime,
}

/// A space ID a second machine reports while another owns it. The hub
/// lists and routes to the owner's space only, until it ends there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    pub space: SpaceId,
    /// The name of the copy.
    pub name: SpaceName,
    pub owner: MemberId,
    /// The machine whose copy the hub leaves out.
    pub copy: MemberId,
    pub since: UnixTime,
}

/// The hub's directory: its machines, every space they reported, and the
/// space IDs two of them report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Directory {
    /// The joined machines, the hub's own among them, with when each was
    /// last seen.
    pub machines: Vec<Member>,
    pub spaces: Vec<Listed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<Conflict>,
}

impl Directory {
    /// The machine `id`.
    pub fn machine(&self, id: &MemberId) -> Option<&Member> {
        self.machines.iter().find(|machine| &machine.id == id)
    }
}

/// What a machine's callers see of the hub's directory: the spaces of the
/// other machines that they see, and this machine's ID at the hub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubView {
    pub machine: MemberId,
    #[serde(flatten)]
    pub directory: Directory,
}
