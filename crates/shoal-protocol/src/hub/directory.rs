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
    /// Its number, from the hub's one sequence across its machines.
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
    /// Whether it has an agent, which receives what the spaces below it
    /// pass up and decides their requests.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub agent: bool,
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

impl Listed {
    /// The space without its questions and approval requests, which it
    /// never copies.
    pub fn without_requests(&self) -> Self {
        let space = &self.space;
        Self {
            machine: self.machine.clone(),
            space: SpaceSummary {
                id: space.id.clone(),
                number: space.number,
                name: space.name.clone(),
                kind: space.kind,
                repository: space.repository.clone(),
                parent: space.parent.clone(),
                mode: space.mode.clone(),
                status: space.status,
                agent_state: space.agent_state,
                agent: space.agent,
                progress: space.progress.clone(),
                questions: Vec::new(),
                approvals: Vec::new(),
            },
            reported_at: self.reported_at,
        }
    }
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

/// A piece of a [`HubView`], which a response carries in pages, since the
/// spaces of every machine may not fit in one frame, nor one space's
/// questions and approval requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::large_enum_variant, reason = "a view holds a few spaces")]
pub enum ViewPart {
    /// This machine's ID at the hub, which comes first.
    Here(MemberId),
    Machine(Member),
    /// A space without its questions and approval requests, which follow
    /// it.
    Space(Listed),
    Question(Question),
    Approval(ApprovalRequest),
    Conflict(Conflict),
}

impl HubView {
    /// The view in pieces, this machine's ID first.
    pub fn into_parts(self) -> Vec<ViewPart> {
        let Directory {
            machines,
            spaces,
            conflicts,
        } = self.directory;
        (std::iter::once(ViewPart::Here(self.machine)))
            .chain(machines.into_iter().map(ViewPart::Machine))
            .chain(spaces.into_iter().flat_map(space_parts))
            .chain(conflicts.into_iter().map(ViewPart::Conflict))
            .collect()
    }

    /// The view `parts` hold, in the order [`Self::into_parts`] gives them;
    /// none for no parts, from a machine that takes no part in a hub.
    pub fn from_parts(parts: Vec<ViewPart>) -> anyhow::Result<Option<Self>> {
        let mut parts = parts.into_iter();
        let machine = match parts.next() {
            None => return Ok(None),
            Some(ViewPart::Here(machine)) => machine,
            Some(_) => anyhow::bail!("a hub view starts with this machine's ID"),
        };
        let mut directory = Directory::default();
        for part in parts {
            match part {
                ViewPart::Here(_) => anyhow::bail!("a hub view names this machine once"),
                ViewPart::Machine(member) => directory.machines.push(member),
                ViewPart::Space(listed) => directory.spaces.push(listed),
                ViewPart::Question(question) => asking(&mut directory, &question.space_id)?
                    .questions
                    .push(question),
                ViewPart::Approval(request) => asking(&mut directory, &request.space_id)?
                    .approvals
                    .push(request),
                ViewPart::Conflict(conflict) => directory.conflicts.push(conflict),
            }
        }
        Ok(Some(Self { machine, directory }))
    }
}

/// `listed` as parts: the space, then each of its questions and approval
/// requests.
fn space_parts(mut listed: Listed) -> Vec<ViewPart> {
    let questions = std::mem::take(&mut listed.space.questions);
    let approvals = std::mem::take(&mut listed.space.approvals);
    (std::iter::once(ViewPart::Space(listed)))
        .chain(questions.into_iter().map(ViewPart::Question))
        .chain(approvals.into_iter().map(ViewPart::Approval))
        .collect()
}

/// The space `id` that a question or request follows: the last one
/// `directory` took.
fn asking<'a>(directory: &'a mut Directory, id: &SpaceId) -> anyhow::Result<&'a mut SpaceSummary> {
    match directory.spaces.last_mut() {
        Some(listed) if &listed.space.id == id => Ok(&mut listed.space),
        _ => anyhow::bail!("a hub view sends a question or request after its space {id}"),
    }
}

#[cfg(test)]
mod tests;
