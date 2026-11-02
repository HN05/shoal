//! Starting a new workspace's preparation in one request, as `shoal add`
//! and `shoal split` do in several: the checks before anything is created,
//! the workspace and its issue, then the operation that prepares it.
use std::sync::Arc;

use anyhow::Result;

use crate::{
    agent::Agent,
    config::Effective,
    daemon::{links::issue::OpenIssue, log, scope::Caller, workspace::Manager},
    model::Workspace,
    protocol::operations::{OperationSpec, Preparation, QueuedLaunch, StartedWorkspace},
    removal::{InspectionPolicy, UnsavedWork},
    state::WorkspaceState,
};

/// What preparing a new workspace does once it exists.
pub(crate) struct Preparing {
    /// An issue number or URL the workspace is assigned.
    pub issue: Option<String>,
    pub pr: Option<String>,
    pub agent: Option<QueuedLaunch>,
}

impl Manager {
    /// Create workspace `name` in `repository` from `base` and start
    /// preparing it.
    pub(crate) async fn start_workspace(
        self: &Arc<Self>,
        repository: String,
        name: String,
        base: Option<String>,
        preparing: Preparing,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        if let Some(agent) = &preparing.agent {
            check_agent(agent, &self.repository_settings(&repository).await?)?;
        }
        let issue = match &preparing.issue {
            Some(input) => {
                let checkout = self.repository(repository.as_str()).await?.path;
                Some(self.read_open_issue(&checkout, input).await?)
            }
            None => None,
        };
        let workspace = self
            .create_workspace_for_chat(repository.into(), name, base, Default::default(), None)
            .await?;
        self.prepare_new(workspace, issue, preparing, caller).await
    }

    /// Split child `name` from workspace `parent`, on `base`, and start
    /// preparing it.
    pub(crate) async fn start_split(
        self: &Arc<Self>,
        parent: &str,
        name: String,
        base: Option<String>,
        preparing: Preparing,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        let parent = self.workspace(parent).await?;
        if let Some(agent) = &preparing.agent {
            check_agent(agent, &self.workspace_settings(&parent).await?)?;
        }
        let issue = match &preparing.issue {
            Some(input) => Some(self.read_open_issue(&parent.path, input).await?),
            None => None,
        };
        let child = self.split_workspace(&parent.id, name, base).await?;
        self.prepare_new(child, issue, preparing, caller).await
    }

    /// Assign `issue` to the new `workspace`, then start preparing it: its
    /// setup while it is preparing, its post-setup hook, then linking the PR
    /// and queuing the agent. A workspace whose issue cannot be assigned is
    /// removed, since nothing ran in it yet.
    async fn prepare_new(
        self: &Arc<Self>,
        workspace: Workspace,
        issue: Option<OpenIssue>,
        preparing: Preparing,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        if let Some(OpenIssue { url, title }) = issue
            && let Err(error) = self.set_issue(&workspace.id, &url, Some(title)).await
        {
            self.discard_new(&workspace).await;
            return Err(error);
        }
        let steps = Preparation {
            setup: workspace.state == WorkspaceState::Preparing,
            post_setup: true,
            pr: preparing.pr,
            agent: preparing.agent,
        };
        let spec = OperationSpec::Prepare {
            workspace: workspace.id.clone(),
            steps,
        };
        let operation = self.start_operation(spec, None, caller).await?;
        Ok(StartedWorkspace {
            operation,
            workspace: Box::new(self.workspace(&workspace.id).await?),
        })
    }

    /// Remove the new `workspace`, which nothing ran in yet.
    async fn discard_new(&self, workspace: &Workspace) {
        let removal = self
            .remove_workspace(&workspace.id, UnsavedWork::All, InspectionPolicy::GitOnly)
            .await;
        if let Err(error) = removal {
            log!(
                "cannot remove {} after its creation failed: {error:#}",
                workspace.name
            );
        }
    }
}

/// Refuse `agent` when it cannot run under `settings`, as `shoal add` does
/// before anything is created.
pub(crate) fn check_agent(agent: &QueuedLaunch, settings: &Effective) -> Result<()> {
    let parsed: Agent = agent.agent.parse()?;
    crate::cli::agents::ensure_available(parsed.name(), settings)
}
