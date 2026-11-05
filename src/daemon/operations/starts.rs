//! Opening a workspace and starting its preparation in one request, as
//! `shoal start` and `shoal split` do: the checks before anything is created,
//! the workspace and its issue, then the operation that prepares it.
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Result, ensure};

use crate::{
    cli::commands::check_queued_launch,
    daemon::{
        links::issue::OpenIssue,
        log,
        scope::Caller,
        workspace::{Manager, NewWorkspace},
    },
    model::{OpenedWorkspace, Workspace},
    protocol::{
        CreationOptions,
        operations::{OperationSpec, Preparation, QueuedLaunch, StartedWorkspace, WorkspaceStart},
    },
    removal::{InspectionPolicy, UnsavedWork},
    repository::Selector,
    state::WorkspaceState,
};

/// What a workspace [`Manager::start_workspace`] opens: where it starts,
/// from what base, where and with what Git profile, below which space, and
/// what it leaves out.
pub(crate) struct Opening<'a> {
    pub start: WorkspaceStart,
    pub base: Option<String>,
    pub new: NewWorkspace<'a>,
    pub parent: Option<String>,
}

/// What preparing a new workspace does once it exists, in the caller's
/// environment `env`.
pub(crate) struct Preparing {
    /// An issue number or URL the workspace is assigned.
    pub issue: Option<String>,
    pub pr: Option<String>,
    pub agent: Option<Box<QueuedLaunch>>,
    pub env: Option<BTreeMap<String, String>>,
}

impl Manager {
    /// Open the workspace `opening` names in `repository`, a new one or
    /// the one on its branch or PR head, and start preparing it.
    pub(crate) async fn start_workspace(
        self: &Arc<Self>,
        repository: Selector,
        opening: Opening<'_>,
        preparing: Preparing,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        let repo = self.repository(repository.clone()).await?;
        if let Some(agent) = &preparing.agent {
            ensure!(
                !matches!(opening.start, WorkspaceStart::PullHead { .. }),
                "a PR's head opens in a workspace at a commit, which holds no agent of its own"
            );
            check_queued_launch(agent, &self.repository_settings(&repo.id).await?)?;
        }
        let issue = match &preparing.issue {
            Some(input) => Some(self.read_open_issue(&repo.path, input).await?),
            None => None,
        };
        let opened = (self.open_for_start(repository, opening, caller.is_some())).await?;
        self.prepare_new(opened, issue, preparing, caller).await
    }

    /// Split child `name` from workspace `parent`, on `base`, leaving out
    /// what `options` say, and start preparing it.
    pub(crate) async fn start_split(
        self: &Arc<Self>,
        parent: &str,
        name: String,
        base: Option<String>,
        options: CreationOptions,
        preparing: Preparing,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        let parent = self.workspace(parent).await?;
        if let Some(agent) = &preparing.agent {
            check_queued_launch(agent, &self.workspace_settings(&parent).await?)?;
        }
        let issue = match &preparing.issue {
            Some(input) => Some(self.read_open_issue(&parent.path, input).await?),
            None => None,
        };
        let child = self
            .split_workspace(&parent.id, name, base, options)
            .await?;
        let opened = OpenedWorkspace {
            workspace: child,
            reused: false,
        };
        self.prepare_new(opened, issue, preparing, caller).await
    }

    /// Create the workspace `opening` names in `repository` on a new
    /// branch, or open the one on its existing branch or PR head, or the
    /// recorded one it names. A `confined` parent, whose own agent asks,
    /// takes over no workspace outside its subtree.
    async fn open_for_start(
        &self,
        repository: Selector,
        Opening {
            start,
            base,
            new,
            parent,
        }: Opening<'_>,
        confined: bool,
    ) -> Result<OpenedWorkspace> {
        let placed = base.is_some() || new.path.is_some() || new.git_profile.is_some();
        match start {
            WorkspaceStart::Name(name) => {
                let workspace = self
                    .create_workspace_below(repository, name, base, new, parent)
                    .await?;
                Ok(OpenedWorkspace {
                    workspace,
                    reused: false,
                })
            }
            WorkspaceStart::Branch(branch) => {
                self.open_branch_below(repository, &branch, base, new, parent, confined)
                    .await
            }
            WorkspaceStart::PullHead { url, head } => {
                ensure!(
                    !placed,
                    "a workspace at a PR's head takes no base, path or Git profile"
                );
                self.open_pull_head(repository, url, head, parent, new.options, confined)
                    .await
            }
            WorkspaceStart::Workspace(workspace) => {
                ensure!(
                    !placed && parent.is_none(),
                    "an existing workspace takes no base, path, Git profile or parent"
                );
                let repository = self.repository(repository).await?;
                let workspace = self.workspace(&workspace).await?;
                ensure!(
                    workspace.repository_id == repository.id,
                    "workspace {} is not in the requested repository",
                    workspace.name
                );
                self.reopen_workspace(&workspace.id).await
            }
        }
    }

    /// Assign `issue` to the `opened` workspace, then start preparing it:
    /// its setup while it is preparing, linking the PR, the post-setup hook
    /// of a new workspace, then queuing the agent. A new workspace whose
    /// issue cannot be assigned is removed, since nothing ran in it yet.
    async fn prepare_new(
        self: &Arc<Self>,
        OpenedWorkspace { workspace, reused }: OpenedWorkspace,
        issue: Option<OpenIssue>,
        preparing: Preparing,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        if let Some(OpenIssue { url, title }) = issue
            && let Err(error) = self.set_issue(&workspace.id, &url, Some(title)).await
        {
            if !reused {
                self.discard_new(&workspace).await;
            }
            return Err(error);
        }
        let steps = Preparation {
            setup: workspace.state == WorkspaceState::Preparing,
            post_setup: !reused,
            pr: preparing.pr,
            agent: preparing.agent,
        };
        let spec = OperationSpec::Prepare {
            workspace: workspace.id.clone(),
            steps,
        };
        let operation = self.start_operation(spec, preparing.env, caller).await?;
        Ok(StartedWorkspace {
            operation,
            workspace: Box::new(self.workspace(&workspace.id).await?),
            reused,
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
