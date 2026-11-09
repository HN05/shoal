//! What a command needs of the workspace it acts on, so completion and
//! pickers can say why a workspace would be refused instead of offering it
//! as if it would work.
use crate::execution_ids::ExecutionId;
use crate::space_ids::SpaceId;
use std::collections::HashSet;

use crate::space_names::SpaceSelector;
use crate::{connection::Daemon, list_status::ListStatus, model::Workspace, state::WorkspaceState};

/// The condition a command's workspace must meet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Need {
    /// Any registered workspace, such as for reading its records.
    #[default]
    Any,
    /// A ready workspace; changes and executions require one.
    Ready,
    /// A ready workspace, or a failed one an interrupted operation left to finish.
    Settled,
    /// A workspace that accepts issue and PR link changes.
    Links,
    /// A workspace setup can reserve: preparing, ready or failed, with no
    /// execution running besides the caller's own.
    Setup,
    /// A workspace a checkpoint can be taken of: ready, stopping or failed.
    Checkpoint,
    /// A worktree directory to enter.
    Directory,
    /// An agent session to attach to.
    Session,
    /// A running agent or command to stop.
    Running,
    /// A space whose untracked processes to stop: a settled workspace, or a
    /// running chat.
    Untracked,
    /// A ready workspace with a recorded completion to withdraw.
    Done,
    /// A recorded base workspace to clear.
    Base,
    /// A hold to release.
    Holds,
    /// A ready-for-review mark to withdraw.
    Marks,
}

/// How a command's need judges one workspace.
#[derive(Debug, PartialEq, Eq)]
pub enum Fit {
    /// The command would act on it.
    Accepted,
    /// The command would refuse it for a reason worth showing, such as its
    /// state or a running execution.
    Refused(String),
    /// It lacks what the command acts on, such as an agent session to attach
    /// to; pickers and completion leave it out.
    Hidden,
}

/// A need with the live facts it is judged against.
pub struct Availability {
    pub need: Need,
    /// Workspace IDs with agent sessions, loaded only for [`Need::Session`].
    pub sessions: HashSet<SpaceId>,
    /// Workspace IDs with an execution that blocks setup, loaded only for
    /// [`Need::Setup`].
    pub busy: HashSet<SpaceId>,
}

impl Availability {
    /// The facts `need` is judged against: for [`Need::Session`], the IDs
    /// of the workspaces with agent sessions, which `sessions` finds where
    /// the caller's terminal hosts keep them.
    pub async fn load(
        need: Need,
        daemon: &impl Daemon,
        workspaces: &[Workspace],
        sessions: impl AsyncFnOnce() -> HashSet<SpaceId>,
    ) -> Self {
        let sessions = if need == Need::Session {
            sessions().await
        } else {
            HashSet::new()
        };
        let busy = if need == Need::Setup {
            busy_workspaces(daemon, workspaces).await
        } else {
            HashSet::new()
        };
        Self {
            need,
            sessions,
            busy,
        }
    }

    /// How the command would treat `workspace`.
    pub fn fit(&self, workspace: &Workspace) -> Fit {
        let state = workspace.state;
        let accepts = |states: &[WorkspaceState]| states.contains(&state);
        let lacks = match self.need {
            Need::Directory => !workspace.path.is_dir(),
            Need::Session => !self.sessions.contains(&workspace.id),
            Need::Running => !workspace.running,
            Need::Done => !workspace.done,
            Need::Base => workspace.base_workspace.is_none(),
            Need::Holds => workspace.holds.is_empty(),
            Need::Marks => workspace.review.is_empty(),
            Need::Any
            | Need::Ready
            | Need::Settled
            | Need::Untracked
            | Need::Links
            | Need::Setup
            | Need::Checkpoint => false,
        };
        let settled = match self.need {
            Need::Ready | Need::Done => accepts(&[WorkspaceState::Ready]),
            Need::Settled | Need::Untracked => {
                accepts(&[WorkspaceState::Ready, WorkspaceState::Failed])
            }
            Need::Links => state.accepts_links(),
            Need::Setup => accepts(&[
                WorkspaceState::Preparing,
                WorkspaceState::Ready,
                WorkspaceState::Failed,
            ]),
            Need::Checkpoint => accepts(&[
                WorkspaceState::Ready,
                WorkspaceState::Stopping,
                WorkspaceState::Failed,
            ]),
            Need::Any
            | Need::Directory
            | Need::Session
            | Need::Running
            | Need::Base
            | Need::Holds
            | Need::Marks => true,
        };
        if lacks {
            Fit::Hidden
        } else if !settled {
            // A workspace that is not ready is named by its state, as lists do.
            Fit::Refused(
                ListStatus::of_workspace(workspace, false)
                    .label()
                    .to_owned(),
            )
        } else if self.need == Need::Setup && self.busy.contains(&workspace.id) {
            Fit::Refused("running".to_owned())
        } else {
            Fit::Accepted
        }
    }
}

/// Workspaces whose running executions would block setup. Setup exempts
/// the caller's own execution, so the caller's workspace counts only with
/// another execution, as its inspection lists them.
async fn busy_workspaces(daemon: &impl Daemon, workspaces: &[Workspace]) -> HashSet<SpaceId> {
    let own = std::env::var(crate::env::EXECUTION_ID)
        .ok()
        .map(ExecutionId::from);
    let mut busy = HashSet::new();
    for workspace in workspaces.iter().filter(|w| w.running) {
        let blocking = match &own {
            Some(own) => crate::workspaces::inspect(daemon, SpaceSelector::id(&workspace.id))
                .await
                .map_or(true, |inspection| {
                    inspection.executions.iter().any(|e| &e.id != own)
                }),
            None => true,
        };
        if blocking {
            busy.insert(workspace.id.clone());
        }
    }
    busy
}
