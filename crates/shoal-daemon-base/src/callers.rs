//! Who sends the daemon a request: the space, and the tracked execution if
//! any, that its scope token belongs to. `None` in its place is the user.
use crate::config::permissions::PermissionKey;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::executions::ExecutionKind;
use shoal_protocol::hub::routing::VouchedSpace;
use shoal_protocol::reviews::ReviewId;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

/// The space and optional tracked execution a scope token belongs to.
#[derive(Debug, Clone)]
pub struct Caller {
    pub space_id: SpaceId,
    pub execution: Option<ScopedExecution>,
    /// The permission whose action an approval let this caller's command
    /// take, which lets the requests that command sends take actions that
    /// permission governs without asking again.
    pub granted: Option<PermissionKey>,
    /// What it may change of what its scope reaches.
    pub access: Access,
    /// What another machine vouches for this caller, a process of a space
    /// there, with the request it forwarded: the reach of its scope and its
    /// permissions, which decide that request and no other.
    pub vouched: Option<Arc<VouchedSpace>>,
}

/// What a caller may change of what its scope reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// What its scope and permissions allow.
    Full,
    /// Nothing: it only reads, as the gate's runs do, and is refused every
    /// request that would change anything.
    ReadOnly,
    /// Only the running review it writes, as a local review's run does:
    /// it reads as a read-only caller does, and opens that review's
    /// findings, reopens a fixed one and submits it.
    Review(ReviewId),
    /// Only a post in the thread whose record is this, as the run that
    /// answers a holder's post in a finding's thread makes: it reads as a
    /// read-only caller does, and posts there for Shoal, which opened it.
    Answer(i64),
}

impl Access {
    /// The running review it writes, none for any other access.
    pub fn review(self) -> Option<ReviewId> {
        match self {
            Self::Review(id) => Some(id),
            Self::Full | Self::ReadOnly | Self::Answer(_) => None,
        }
    }

    /// The record of the thread it answers in, none for any other access.
    pub fn answer(self) -> Option<i64> {
        match self {
            Self::Answer(thread) => Some(thread),
            Self::Full | Self::ReadOnly | Self::Review(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScopedExecution {
    pub id: ExecutionId,
    pub kind: ExecutionKind,
    /// The agent execution whose process tree registered this one, as a
    /// command an agent runs with `shoal run`.
    pub agent_execution: Option<ExecutionId>,
}

impl Caller {
    /// A process of the space `space_id` that work the daemon carries out
    /// later acts for, such as a queued launch, with that space's scope
    /// and no execution of its own.
    pub fn of_space(space_id: SpaceId) -> Self {
        Self {
            space_id,
            execution: None,
            granted: None,
            access: Access::Full,
            vouched: None,
        }
    }

    pub fn execution_id(&self) -> Option<&ExecutionId> {
        self.execution.as_ref().map(|execution| &execution.id)
    }

    pub fn kind(&self) -> Option<ExecutionKind> {
        self.execution.as_ref().map(|execution| execution.kind)
    }

    /// The space an optional filter names, which defaults to the caller's
    /// own.
    pub fn own_by_default<'a>(
        &self,
        workspace: &'a mut Option<SpaceSelector>,
    ) -> &'a mut SpaceSelector {
        workspace.get_or_insert_with(|| SpaceSelector::id(&self.space_id))
    }
}
