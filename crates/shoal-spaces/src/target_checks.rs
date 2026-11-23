//! What each request requires of the space it acts on: one check per
//! request kind, which every request group declares with no default arm.
//! The daemon runs it before the request's handler, and a handler that
//! reads the space again under a lock judges it by the same check, so both
//! refuse a space alike.
use crate::Spaces;
use crate::daemon::Daemon;
use std::collections::HashSet;

use anyhow::Result;
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};

use super::scope::Caller;
use crate::{
    model::{Space, SpaceKind, SpacePart, Worktree},
    protocol::Target,
    refusal::{Reason, Refusal},
    state::WorkspaceState,
};

/// What a request requires of the space it acts on, each judged by a
/// named operation below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetCheck {
    /// Any workspace or chat, running or stopped.
    Space,
    /// A workspace; a chat lacks the worktree the request acts on.
    Workspace,
    /// A ready workspace.
    ReadyWorkspace,
    /// A ready workspace, or a chat.
    ReadySpace,
    /// A ready workspace, or one a lifecycle operation that passes holds,
    /// which the request waits out; or a chat.
    AwaitedReadySpace,
    /// A workspace its setup can run in: preparing, ready or failed, with no
    /// execution in it but the caller's own.
    SetupWorkspace,
    /// A ready workspace, or a failed one an interrupted operation left
    /// for the request to finish.
    SettledWorkspace,
    /// A ready or failed workspace, one whose preparation waits with
    /// nothing started in it, or a chat.
    SettledSpace,
    /// A workspace that takes link changes, or a chat.
    LinkingSpace,
    /// A workspace that takes link changes.
    LinkingWorkspace,
    /// A workspace a checkpoint can be taken of: ready, stopping or
    /// failed.
    CheckpointedWorkspace,
    /// A space where an agent or command runs; the request acts on nothing
    /// in another.
    RunningSpace,
    /// A ready workspace or a chat that recorded its completion.
    DoneSpace,
    /// A space with a hold to release.
    HeldSpace,
    /// A space with a ready-for-review mark to withdraw.
    MarkedSpace,
    /// A workspace that records a base workspace to clear.
    StackedWorkspace,
    /// A space with an agent session to attach to, where this machine's
    /// terminal hosts keep it.
    SessionSpace,
    /// A space whose agent's directory is found on this machine, for a
    /// shell or an app to enter.
    DirectorySpace,
}

/// What a check reads besides the space it judges, which
/// [`Manager::facts`] loads only for a check that reads it.
#[derive(Debug, Default)]
pub struct Facts {
    /// The spaces where work runs other than the caller's own execution,
    /// which a setup leaves running.
    pub busy: HashSet<SpaceId>,
    /// The spaces with an agent session.
    pub sessions: HashSet<SpaceId>,
    /// The spaces whose agent's directory is found.
    pub directories: HashSet<SpaceId>,
}

/// How a request's check judges one space.
#[derive(Debug)]
pub enum Verdict {
    /// The request acts on it.
    Accepted,
    /// The request refuses it with this error, for a reason worth showing,
    /// such as its state.
    Refused(anyhow::Error),
    /// It lacks what the request acts on, such as a worktree or a running
    /// agent to stop, so it is no candidate for it. The request is refused
    /// with the error where there is one, and otherwise its handler says
    /// what becomes of it.
    Lacking(Option<anyhow::Error>),
}

impl Verdict {
    /// Refuse a request this verdict refuses.
    pub fn require(self) -> Result<()> {
        match self {
            Self::Accepted | Self::Lacking(None) => Ok(()),
            Self::Refused(error) | Self::Lacking(Some(error)) => Err(error),
        }
    }
}

impl TargetCheck {
    /// Whether the request path reads the space to judge it: a check that
    /// can refuse what the handler would not refuse first. Every recorded
    /// space passes `Space`; a workspace check refuses only a chat, which
    /// its handler refuses through `Manager::workspace` alike; and a check
    /// of what a space has only leaves out one without it, which its handler
    /// answers.
    fn refuses_before_handler(self) -> bool {
        match self {
            Self::Space
            | Self::Workspace
            | Self::RunningSpace
            | Self::HeldSpace
            | Self::MarkedSpace
            | Self::StackedWorkspace
            | Self::SessionSpace
            | Self::DirectorySpace => false,
            Self::ReadyWorkspace
            | Self::ReadySpace
            | Self::AwaitedReadySpace
            | Self::SetupWorkspace
            | Self::SettledWorkspace
            | Self::SettledSpace
            | Self::LinkingSpace
            | Self::LinkingWorkspace
            | Self::CheckpointedWorkspace
            | Self::DoneSpace => true,
        }
    }

    /// How a request of this check judges `space`, with `facts`.
    pub fn judge(self, space: &Space, facts: &Facts) -> Verdict {
        match self {
            Self::Space => Verdict::Accepted,
            Self::Workspace => workspace(space, |_| Verdict::Accepted),
            Self::ReadyWorkspace => workspace(space, |w| ready(space, w.state)),
            Self::ReadySpace => ready_space(space),
            Self::AwaitedReadySpace => awaited_ready(space),
            Self::SetupWorkspace => setup(space, facts.busy.contains(&space.id)),
            Self::SettledWorkspace => workspace(space, |w| settled(space, w.state)),
            Self::SettledSpace => settled_space(space),
            Self::LinkingSpace => linking_space(space),
            Self::LinkingWorkspace => workspace(space, |w| linking(w.state)),
            Self::CheckpointedWorkspace => workspace(space, |w| checkpointed(w.state)),
            Self::RunningSpace => present(space.running),
            Self::DoneSpace => done_space(space),
            Self::HeldSpace => present(!space.holds.is_empty()),
            Self::MarkedSpace => present(!space.review.is_empty()),
            Self::StackedWorkspace => workspace(space, |w| present(w.base_workspace.is_some())),
            Self::SessionSpace => present(facts.sessions.contains(&space.id)),
            Self::DirectorySpace => present(facts.directories.contains(&space.id)),
        }
    }
}

/// A workspace judged by `judged` on its worktree's record; a chat lacks
/// the worktree, refused as [`Manager::workspace`] refuses it.
fn workspace(space: &Space, judged: impl FnOnce(&Worktree) -> Verdict) -> Verdict {
    match &space.part {
        SpacePart::Workspace(worktree) => judged(worktree),
        SpacePart::Chat(_) => Verdict::Lacking(Some(other_kind(space, SpaceKind::Workspace))),
    }
}

/// Accept `workspace` in `state` where it is ready, which a handler that
/// read its state again under a lock asks too.
pub fn ready<P>(workspace: &Space<P>, state: WorkspaceState) -> Verdict {
    match state {
        WorkspaceState::Ready => Verdict::Accepted,
        state => Verdict::Refused(not_ready(workspace, state)),
    }
}

/// Accept `workspace` in `state` where it is ready, or failed with an
/// interrupted operation for the request to finish, which a handler that
/// read its state again under a lock asks too.
pub fn settled<P>(workspace: &Space<P>, state: WorkspaceState) -> Verdict {
    match state {
        WorkspaceState::Ready | WorkspaceState::Failed => Verdict::Accepted,
        state => Verdict::Refused(not_ready(workspace, state)),
    }
}

/// The refusal of `workspace`, in `state`, by a request that needs it
/// ready, as the store's recheck in a transaction words it.
fn not_ready<P>(workspace: &Space<P>, state: WorkspaceState) -> anyhow::Error {
    shoal_store::not_ready(workspace.number, &workspace.id, &workspace.name, state)
}

/// A ready or failed workspace, or one whose preparation waits with
/// nothing started in it; or any chat.
fn settled_space(space: &Space) -> Verdict {
    match &space.part {
        SpacePart::Workspace(worktree) if worktree.preparation_waits.is_some() => Verdict::Accepted,
        SpacePart::Workspace(worktree) => settled(space, worktree.state),
        SpacePart::Chat(_) => Verdict::Accepted,
    }
}

/// A ready workspace, or any chat.
fn ready_space(space: &Space) -> Verdict {
    match &space.part {
        SpacePart::Workspace(worktree) => ready(space, worktree.state),
        SpacePart::Chat(_) => Verdict::Accepted,
    }
}

/// A ready workspace, or one a lifecycle operation that passes holds, which
/// the request waits out; or any chat.
fn awaited_ready(space: &Space) -> Verdict {
    match &space.part {
        SpacePart::Workspace(worktree) if worktree.state.passes() => Verdict::Accepted,
        SpacePart::Workspace(worktree) => ready(space, worktree.state),
        SpacePart::Chat(_) => Verdict::Accepted,
    }
}

/// A workspace its setup can run in: preparing, ready or failed, and not
/// `busy` with work other than the caller's own execution.
fn setup(space: &Space, busy: bool) -> Verdict {
    workspace(space, |worktree| match worktree.state {
        WorkspaceState::Preparing | WorkspaceState::Ready | WorkspaceState::Failed if busy => {
            let number = space.number;
            Verdict::Refused(anyhow::anyhow!(
                "workspace {space} runs work; setup runs once it ends, which shoal stop {number} does"
            ))
        }
        WorkspaceState::Preparing | WorkspaceState::Ready | WorkspaceState::Failed => {
            Verdict::Accepted
        }
        state => Verdict::Refused(not_ready(space, state)),
    })
}

/// A workspace whose state takes link changes, or any chat.
fn linking_space(space: &Space) -> Verdict {
    match &space.part {
        SpacePart::Workspace(worktree) => linking(worktree.state),
        SpacePart::Chat(_) => Verdict::Accepted,
    }
}

/// Accept a workspace in `state` where it takes link changes, which a
/// link change asks again in the transaction that records it.
pub fn linking(state: WorkspaceState) -> Verdict {
    match state.accepts_links() {
        true => Verdict::Accepted,
        false => Verdict::Refused(anyhow::anyhow!(
            "workspace cannot change links while {state}"
        )),
    }
}

/// Accept a workspace in `state` where a checkpoint can be taken of it:
/// ready, stopping or failed, as no other lifecycle operation holds it.
pub fn checkpointed(state: WorkspaceState) -> Verdict {
    match state {
        WorkspaceState::Ready | WorkspaceState::Stopping | WorkspaceState::Failed => {
            Verdict::Accepted
        }
        state => Verdict::Refused(anyhow::anyhow!(
            "workspace is {state}; take a checkpoint once that operation finishes"
        )),
    }
}

/// A space that recorded its completion, which only a ready workspace or a
/// chat withdraws.
fn done_space(space: &Space) -> Verdict {
    let done = match &space.part {
        SpacePart::Workspace(worktree) => worktree.done,
        SpacePart::Chat(chat) => chat.completion.is_some(),
    };
    match done {
        true => ready_space(space),
        false => Verdict::Lacking(None),
    }
}

/// Accept a space that has what the request acts on, and leave its
/// handler to answer for one that has not.
fn present(has: bool) -> Verdict {
    match has {
        true => Verdict::Accepted,
        false => Verdict::Lacking(None),
    }
}

/// The refusal of `space` to a request that needs a space of kind `needed`.
pub fn other_kind(space: &Space, needed: SpaceKind) -> anyhow::Error {
    let message = format!(
        "space {space} is a {}; this command needs a {}",
        space.kind(),
        needed.as_str()
    );
    Refusal::new(Reason::NotFound, message).into()
}

impl Spaces {
    /// Refuse a request whose `target` names a space that fails the
    /// `check` its kind declares. A space this machine has no record of is left to the
    /// handler, which refuses or forwards it as before.
    pub async fn check_target(
        &self,
        daemon: &dyn Daemon,
        check: Option<TargetCheck>,
        target: Option<Target<'_>>,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let Some(check) = check else {
            return Ok(());
        };
        if !check.refuses_before_handler() {
            return Ok(());
        }
        // A space's process acts in its own space whatever an acting
        // target names.
        let selector = match target {
            Some(Target::Space(selector) | Target::Files(selector)) => selector,
            Some(Target::Item(_) | Target::Acting(_)) | None => return Ok(()),
        };
        let Ok(space) = self.space(selector).await else {
            return Ok(());
        };
        let facts = self
            .facts(daemon, caller, check, std::slice::from_ref(&space))
            .await;
        check.judge(&space, &facts).require()
    }

    /// What `check` reads of `spaces` besides their records, for `caller`.
    pub async fn facts(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        check: TargetCheck,
        spaces: &[Space],
    ) -> Facts {
        let own = caller.and_then(Caller::execution_id);
        Facts {
            busy: match check {
                TargetCheck::SetupWorkspace => self.busy_spaces(own).await,
                _ => HashSet::new(),
            },
            sessions: match check {
                TargetCheck::SessionSpace => self.session_spaces(daemon, spaces).await,
                _ => HashSet::new(),
            },
            directories: match check {
                TargetCheck::DirectorySpace => self.directory_spaces(spaces).await,
                _ => HashSet::new(),
            },
        }
    }

    /// The spaces with an execution other than `own`; none where the store
    /// cannot say, which leaves setup to refuse.
    async fn busy_spaces(&self, own: Option<&ExecutionId>) -> HashSet<SpaceId> {
        let (own, records) = (own.cloned(), self.records.clone());
        let busy = self
            .store
            .read(move |db| records.other_executions(db, None, own.as_ref()));
        let busy = busy.await.unwrap_or_default().into_iter();
        busy.map(|(space, _)| space).collect()
    }
}
