//! A new space's preparation, the only step of its start that depends on
//! its kind: a chat has none, and the queue starts its agent; a workspace's
//! creates the worktree it has yet to get, sets it up while it is
//! preparing, links its PR and runs a new workspace's post-setup hook,
//! unless it waits first, and its failure reaches the start's coordinator
//! or the user.
use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::{launches::Started, space_names::SpaceSelector};

use super::{Preparing, home::Recorded, waiting::PreparationStart};
use crate::{
    daemon::{scope::Caller, workspace::Manager},
    model::{QueueAdmission, Space, Workspace},
    protocol::operations::{OperationSpec, Preparation},
    state::WorkspaceState,
};

impl Manager {
    /// Begin the `recorded` space's preparation as `preparing` says, for
    /// `caller`, its agent queued as `admission` says, returning the space
    /// as the start left it, before a preparation changes it, and how its
    /// agent starts.
    pub(super) async fn prepare_space(
        self: &Arc<Self>,
        recorded: &Recorded,
        preparing: Preparing,
        admission: Option<&QueueAdmission>,
        caller: Option<&Caller>,
    ) -> Result<(Space, Started)> {
        let Ok(workspace) = recorded.space.clone().into_workspace() else {
            let space = self.space_by_id(&recorded.space.id).await?;
            let waiting = admission.map_or(0, |admission| admission.waiting);
            return Ok((space, Started::Queued { waiting }));
        };
        let may_wait = preparing.may_wait(&workspace);
        let Preparing {
            pr, waiting, env, ..
        } = preparing;
        let start = PreparationStart {
            steps: Preparation {
                setup: workspace.state == WorkspaceState::Preparing,
                post_setup: !recorded.reused,
                pr,
                agent: None,
                waiting,
            },
            env,
            for_agent: admission.is_some_and(|admission| admission.entry.is_some()),
        };
        let (workspace, operation) =
            (self.begin_preparation(&workspace, start, may_wait, caller)).await?;
        if let Some(operation) = &operation {
            self.report_preparation_failure(&workspace.id, operation);
        }
        let reused = recorded.reused;
        Ok((workspace.into(), Started::Preparing { operation, reused }))
    }

    /// Start the preparation `start` of `workspace` for `caller`, unless
    /// it `may_wait` and what it waits for holds it, returning the
    /// workspace as the start left it, before its preparation changes it,
    /// and the preparation's operation once it starts.
    async fn begin_preparation(
        self: &Arc<Self>,
        workspace: &Workspace,
        start: PreparationStart,
        may_wait: bool,
        caller: Option<&Caller>,
    ) -> Result<(Workspace, Option<String>)> {
        let held = may_wait && self.hold_preparation(workspace, &start, caller).await?;
        let recorded = self.workspace_by_id(&workspace.id).await?;
        if held {
            return Ok((recorded, None));
        }
        let spec = OperationSpec::Prepare {
            workspace: SpaceSelector::id(&workspace.id),
            steps: start.steps,
        };
        let operation = self.start_operation(spec, start.env, caller).await?;
        Ok((recorded, Some(operation)))
    }
}
