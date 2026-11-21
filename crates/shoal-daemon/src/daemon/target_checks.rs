//! What each request requires of the space it acts on: one check per
//! request kind, which every request group declares with no default arm.
//! The daemon runs it before the request's handler, and a handler that
//! reads the space again under a lock judges it by the same check, so both
//! refuse a space alike.
use anyhow::Result;

use super::manager::Manager;
use crate::{
    model::{Space, SpaceKind, SpacePart, Worktree},
    protocol::{Operation, Target},
    refusal::{Reason, Refusal},
    state::WorkspaceState,
};

/// What a request requires of the space it acts on, each judged by a
/// named operation below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetCheck {
    /// Any workspace or chat, running or stopped.
    Space,
    /// A workspace; a chat lacks the worktree the request acts on.
    Workspace,
    /// A ready workspace.
    ReadyWorkspace,
    /// A ready workspace, or a chat.
    ReadySpace,
    /// A ready workspace, or a failed one an interrupted operation left
    /// for the request to finish.
    SettledWorkspace,
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
}

/// How a request's check judges one space.
#[derive(Debug)]
pub(crate) enum Verdict {
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
    pub(crate) fn require(self) -> Result<()> {
        match self {
            Self::Accepted | Self::Lacking(None) => Ok(()),
            Self::Refused(error) | Self::Lacking(Some(error)) => Err(error),
        }
    }
}

/// A request that declares the check of the space it acts on.
pub(crate) trait Checked {
    /// The check of the space the request names, as its target gives it;
    /// `None` for a request kind that never names one.
    fn target_check(&self) -> Option<TargetCheck>;
}

impl Checked for Operation {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::Daemon(method) => method.target_check(),
            Self::Repository(method) => method.target_check(),
            Self::Workspace(method) => method.target_check(),
            Self::Space(method) => method.target_check(),
            Self::Place(method) => method.target_check(),
            Self::Permission(method) => method.target_check(),
            Self::Config(method) => method.target_check(),
            Self::Diff(method) => method.target_check(),
            Self::Merging(method) => method.target_check(),
            Self::Removal(method) => method.target_check(),
            Self::Recovery(method) => method.target_check(),
            Self::Execution(method) => method.target_check(),
            Self::Assignment(method) => method.target_check(),
            Self::Agent(method) => method.target_check(),
            Self::Chat(method) => method.target_check(),
            Self::Launch(method) => method.target_check(),
            Self::Queue(method) => method.target_check(),
            Self::Hold(method) => method.target_check(),
            Self::Demo(method) => method.target_check(),
            Self::File(method) => method.target_check(),
            Self::Item(method) => method.target_check(),
            Self::Ci(method) => method.target_check(),
            Self::Branch(method) => method.target_check(),
            Self::Alert(method) => method.target_check(),
            Self::Conversion(method) => method.target_check(),
            Self::Operations(method) => method.target_check(),
            Self::Thread(method) => method.target_check(),
            Self::Change(method) => method.target_check(),
            Self::Review(method) => method.target_check(),
            Self::Approval(method) => method.target_check(),
            Self::Port(method) => method.target_check(),
            Self::Secret(method) => method.target_check(),
            Self::Resource(method) => method.target_check(),
            Self::Simulator(method) => method.target_check(),
            Self::Container(method) => method.target_check(),
            Self::Subscription(method) => method.target_check(),
            Self::Trigger(method) => method.target_check(),
            Self::Hub(method) => method.target_check(),
            Self::App(method) => method.target_check(),
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
            | Self::StackedWorkspace => false,
            Self::ReadyWorkspace
            | Self::ReadySpace
            | Self::SettledWorkspace
            | Self::LinkingSpace
            | Self::LinkingWorkspace
            | Self::CheckpointedWorkspace
            | Self::DoneSpace => true,
        }
    }

    /// How a request of this check judges `space`.
    pub(crate) fn judge(self, space: &Space) -> Verdict {
        match self {
            Self::Space => Verdict::Accepted,
            Self::Workspace => workspace(space, |_| Verdict::Accepted),
            Self::ReadyWorkspace => workspace(space, |w| ready(space, w.state)),
            Self::ReadySpace => ready_space(space),
            Self::SettledWorkspace => workspace(space, |w| settled(space, w.state)),
            Self::LinkingSpace => linking_space(space),
            Self::LinkingWorkspace => workspace(space, |w| linking(w.state)),
            Self::CheckpointedWorkspace => workspace(space, |w| checkpointed(w.state)),
            Self::RunningSpace => present(space.running),
            Self::DoneSpace => done_space(space),
            Self::HeldSpace => present(!space.holds.is_empty()),
            Self::MarkedSpace => present(!space.review.is_empty()),
            Self::StackedWorkspace => workspace(space, |w| present(w.base_workspace.is_some())),
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
pub(crate) fn ready<P>(workspace: &Space<P>, state: WorkspaceState) -> Verdict {
    match state {
        WorkspaceState::Ready => Verdict::Accepted,
        state => Verdict::Refused(not_ready(workspace, state)),
    }
}

/// Accept `workspace` in `state` where it is ready, or failed with an
/// interrupted operation for the request to finish.
fn settled<P>(workspace: &Space<P>, state: WorkspaceState) -> Verdict {
    match state {
        WorkspaceState::Ready | WorkspaceState::Failed => Verdict::Accepted,
        state => Verdict::Refused(not_ready(workspace, state)),
    }
}

/// The refusal of `workspace`, in `state`, by a request that needs it in
/// another, naming what to do about it.
fn not_ready<P>(workspace: &Space<P>, state: WorkspaceState) -> anyhow::Error {
    let number = workspace.number;
    let remedy = match state {
        WorkspaceState::Failed => {
            format!("shoal status {number} shows why, and shoal setup {number} retries its setup")
        }
        WorkspaceState::Preparing => {
            format!(
                "run the command again once it is ready; shoal status {number} shows how far it got"
            )
        }
        WorkspaceState::Removing => format!(
            "shoal restore {} reopens it once it is removed",
            workspace.id
        ),
        WorkspaceState::Ready
        | WorkspaceState::Stopping
        | WorkspaceState::Reconciling
        | WorkspaceState::Restoring
        | WorkspaceState::Updating => {
            "run the command again once that operation finishes".to_owned()
        }
    };
    let message = format!("workspace {workspace} is {state}; {remedy}");
    Refusal::new(Reason::NotReady, message).into()
}

/// A ready workspace, or any chat.
fn ready_space(space: &Space) -> Verdict {
    match &space.part {
        SpacePart::Workspace(worktree) => ready(space, worktree.state),
        SpacePart::Chat(_) => Verdict::Accepted,
    }
}

/// A workspace whose state takes link changes, or any chat.
fn linking_space(space: &Space) -> Verdict {
    match &space.part {
        SpacePart::Workspace(worktree) => linking(worktree.state),
        SpacePart::Chat(_) => Verdict::Accepted,
    }
}

/// Accept a workspace in `state` where it takes link changes.
fn linking(state: WorkspaceState) -> Verdict {
    match state.accepts_links() {
        true => Verdict::Accepted,
        false => Verdict::Refused(anyhow::anyhow!(
            "workspace cannot change links while {state}"
        )),
    }
}

/// Accept a workspace in `state` where a checkpoint can be taken of it:
/// ready, stopping or failed, as no other lifecycle operation holds it.
pub(crate) fn checkpointed(state: WorkspaceState) -> Verdict {
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
pub(crate) fn other_kind(space: &Space, needed: SpaceKind) -> anyhow::Error {
    let message = format!(
        "space {space} is a {}; this command needs a {}",
        space.kind(),
        needed.as_str()
    );
    Refusal::new(Reason::NotFound, message).into()
}

impl Manager {
    /// Refuse `operation` where the space it names fails the check its kind
    /// declares. A space this machine has no record of is left to the
    /// handler, which refuses or forwards it as before.
    pub(crate) async fn check_target(&self, operation: &Operation) -> Result<()> {
        let Some(check) = operation.target_check() else {
            return Ok(());
        };
        if !check.refuses_before_handler() {
            return Ok(());
        }
        // A space's process acts in its own space whatever an acting
        // target names.
        let selector = match operation.target() {
            Some(Target::Space(selector)) => selector,
            Some(Target::Item(_) | Target::Acting(_)) | None => return Ok(()),
        };
        match self.space(selector).await {
            Ok(space) => check.judge(&space).require(),
            Err(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
