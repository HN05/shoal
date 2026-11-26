//! A new space's preparation, the only step of its start that depends on
//! its kind. A chat has none; a workspace's creates and sets up its worktree
//! unless it waits first.
use crate::Spaces;
use crate::daemon::Daemon;
use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::{protocol::Started, space_names::SpaceSelector};

use super::{Preparing, home::Recorded, waiting::PreparationStart};
use crate::{
    model::{QueueAdmission, Space, Workspace},
    protocol::operations::{OperationSpec, Preparation},
    scope::Caller,
    state::WorkspaceState,
};

impl Spaces {
    /// The agent is queued as `admission` says. Returns the space as the
    /// start left it, before a preparation changes it, and how its agent
    /// starts.
    pub(super) async fn prepare_space(
        &self,
        daemon: &Arc<dyn Daemon>,
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
            (self.begin_preparation(daemon, &workspace, start, may_wait, caller)).await?;
        if let Some(operation) = &operation {
            self.report_preparation_failure(daemon, &workspace.id, operation);
        }
        let reused = recorded.reused;
        Ok((workspace.into(), Started::Preparing { operation, reused }))
    }

    /// Skipped while it `may_wait` and what it waits for holds it. Returns
    /// the workspace as the start left it, before its preparation changes
    /// it, and the preparation's operation once it starts.
    async fn begin_preparation(
        &self,
        daemon: &Arc<dyn Daemon>,
        workspace: &Workspace,
        start: PreparationStart,
        may_wait: bool,
        caller: Option<&Caller>,
    ) -> Result<(Workspace, Option<String>)> {
        let held = may_wait
            && self
                .hold_preparation(&**daemon, workspace, &start, caller)
                .await?;
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
