//! Opening a workspace and starting its preparation in one request, as
//! `shoal start` and `shoal split` do: the checks before anything is created,
//! the workspace and its issue, then the operation that prepares it.
use crate::client::space_ids::SpaceId;
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context as _, Result, ensure};

use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    agents::check_queued_launch,
    daemon::{
        links::issue::OpenIssue,
        log,
        scope::Caller,
        workspace::{CopyPlace, Manager, NewWorkspace},
    },
    model::{OpenedWorkspace, Workspace},
    protocol::{
        CopySource, CreationOptions,
        operations::{OperationSpec, Preparation, QueuedLaunch, StartedWorkspace, WorkspaceStart},
    },
    removal::{InspectionPolicy, UnsavedWork},
    repository::Selector,
    state::WorkspaceState,
};

/// Name a new workspace without a name of its own after `issue`'s title,
/// as every start does, whoever sends it; options apply only to a new
/// workspace, so one reused keeps its name.
fn name_after_issue(options: &mut CreationOptions, issue: Option<&OpenIssue>) {
    if let Some(issue) = issue {
        (options.name).get_or_insert_with(|| SpaceName::derive(&issue.title, "workspace"));
    }
}

/// What a workspace [`Manager::start_workspace`] opens: where it starts,
/// from what base, where and with what Git profile, below which space, and
/// what it leaves out.
pub(crate) struct Opening<'a> {
    pub start: WorkspaceStart,
    pub base: Option<String>,
    pub new: NewWorkspace<'a>,
    pub parent: Option<SpaceId>,
}

/// The child a split creates: on the new branch `name` from `base`, or a
/// copy of `copy`, on `name` or a branch named after the copied one's.
pub(crate) struct Split {
    pub name: Option<String>,
    pub copy: Option<CopySource>,
    pub base: Option<String>,
    pub options: CreationOptions,
}

/// What preparing a new workspace does once it exists, in the caller's
/// environment `env`.
pub(crate) struct Preparing {
    /// An issue number or URL the workspace is assigned.
    pub issue: Option<String>,
    pub pr: Option<String>,
    pub agent: Option<Box<QueuedLaunch>>,
    /// The agent the caller starts itself once setup succeeds, which waits
    /// in the queue should setup fail.
    pub waiting: Option<Box<QueuedLaunch>>,
    pub env: Option<BTreeMap<String, String>>,
}

impl Manager {
    /// Open the workspace `opening` names in `repository`, a new one or
    /// the one on its branch or PR head, and start preparing it.
    pub(crate) async fn start_workspace(
        self: &Arc<Self>,
        repository: Selector,
        mut opening: Opening<'_>,
        preparing: Preparing,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        let repo = self.repository(repository.clone()).await?;
        if let Some(agent) = preparing.agent.as_ref().or(preparing.waiting.as_ref()) {
            ensure!(
                !matches!(opening.start, WorkspaceStart::PullHead { .. }),
                "a PR's head opens in a workspace at a commit, which holds no agent of its own"
            );
            let settings = self.repository_settings_with_templates(&repo.id).await?;
            check_queued_launch(agent, &settings)?;
        }
        let issue = match &preparing.issue {
            Some(input) => Some(self.read_open_issue(&repo.path, input).await?),
            None => None,
        };
        name_after_issue(&mut opening.new.options, issue.as_ref());
        if opening.new.options.name.is_none() {
            let title = self
                .opened_pull_title(&repo.path, &opening, &preparing)
                .await;
            opening.new.options.name = title.map(|title| SpaceName::derive(&title, "workspace"));
        }
        let opened = (self.open_for_start(repository, opening, caller)).await?;
        self.prepare_new(opened, issue, preparing, caller).await
    }

    /// The title of the PR a start opens at its head or links, which names
    /// a new workspace without a name or issue of its own, as every start
    /// does; a PR that cannot be read leaves it named after its branch, and
    /// linking it reports why.
    async fn opened_pull_title(
        &self,
        checkout: &std::path::Path,
        opening: &Opening<'_>,
        preparing: &Preparing,
    ) -> Option<String> {
        let input = match &opening.start {
            WorkspaceStart::PullHead { url, .. } => url.as_str(),
            _ => preparing.pr.as_deref()?,
        };
        match self.read_pull_title(checkout, input).await {
            Ok(title) => Some(title),
            Err(error) => {
                log!("PR {input} not read to name its workspace: {error:#}");
                None
            }
        }
    }

    /// Split child `name` from workspace `parent`, on `base` or a copy of
    /// `copy`, leaving out what `options` say, and start preparing it.
    pub(crate) async fn start_split(
        self: &Arc<Self>,
        parent: &SpaceSelector,
        mut split: Split,
        preparing: Preparing,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        let parent = self.workspace(parent).await?;
        if let Some(agent) = &preparing.agent {
            let settings = self.workspace_settings_with_templates(&parent).await?;
            check_queued_launch(agent, &settings)?;
        }
        let issue = match &preparing.issue {
            Some(input) => Some(self.read_open_issue(&parent.path, input).await?),
            None => None,
        };
        name_after_issue(&mut split.options, issue.as_ref());
        let child = self.create_child(&parent, split).await?;
        let opened = OpenedWorkspace {
            workspace: child,
            reused: false,
        };
        self.prepare_new(opened, issue, preparing, caller).await
    }

    /// Create the child `split` describes below `parent`.
    async fn create_child(
        &self,
        parent: &Workspace,
        Split {
            name,
            copy,
            base,
            options,
        }: Split,
    ) -> Result<Workspace> {
        let Some(copy) = copy else {
            let name = name.context("name the child's branch")?;
            return self
                .split_workspace(&SpaceSelector::id(&parent.id), name, base, options)
                .await;
        };
        ensure!(base.is_none(), "a copy starts from its source, not a base");
        let new = NewWorkspace {
            options,
            parent: Some(parent.id.clone()),
            ..Default::default()
        };
        let repository = Selector::from(parent.repository_id.clone());
        (self.create_copy(repository, &copy, CopyPlace::Branch(name), new)).await
    }

    /// Create the workspace `opening` names in `repository` on a new
    /// branch, or open the one on its existing branch or PR head, or the
    /// recorded one it names. For a scoped `caller`, the parent takes over
    /// no workspace outside its subtree.
    async fn open_for_start(
        &self,
        repository: Selector,
        Opening {
            start,
            base,
            new,
            parent,
        }: Opening<'_>,
        caller: Option<&Caller>,
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
                self.open_branch_below(repository, &branch, base, new, parent, caller)
                    .await
            }
            WorkspaceStart::Copy { source, branch } => {
                ensure!(base.is_none(), "a copy starts from its source, not a base");
                let new = NewWorkspace {
                    parent: parent.clone(),
                    ..new
                };
                let place = CopyPlace::Branch(branch);
                let workspace = self.create_copy(repository, &source, place, new).await?;
                Ok(OpenedWorkspace {
                    workspace: self
                        .give_to_parent(&SpaceSelector::id(&workspace.id), parent)
                        .await?,
                    reused: false,
                })
            }
            WorkspaceStart::PullHead { url, head } => {
                ensure!(
                    !placed,
                    "a workspace at a PR's head takes no base, path or Git profile"
                );
                self.open_pull_head(repository, url, head, parent, new.options, caller)
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
                self.reopen_workspace(&SpaceSelector::id(&workspace.id))
                    .await
            }
        }
    }

    /// Assign `issue` to the `opened` workspace, then start preparing it:
    /// its setup while it is preparing, linking the PR, the post-setup hook
    /// of a new workspace, then queuing the agent. A new workspace whose
    /// issue cannot be assigned is removed, since nothing ran in it yet.
    pub(crate) async fn prepare_new(
        self: &Arc<Self>,
        OpenedWorkspace { workspace, reused }: OpenedWorkspace,
        issue: Option<OpenIssue>,
        preparing: Preparing,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        if let Some(OpenIssue { url, title }) = issue
            && let Err(error) = self
                .set_issue(&SpaceSelector::id(&workspace.id), &url, Some(title))
                .await
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
            waiting: preparing.waiting,
        };
        let spec = OperationSpec::Prepare {
            workspace: SpaceSelector::id(&workspace.id),
            steps,
        };
        let operation = self.start_operation(spec, preparing.env, caller).await?;
        Ok(StartedWorkspace {
            operation,
            workspace: Box::new(self.workspace_by_id(&workspace.id).await?),
            reused,
        })
    }

    /// Remove the new `workspace`, which nothing ran in yet.
    async fn discard_new(&self, workspace: &Workspace) {
        let removal = self
            .remove_workspace(
                &SpaceSelector::id(&workspace.id),
                UnsavedWork::All,
                InspectionPolicy::GitOnly,
            )
            .await;
        if let Err(error) = removal {
            log!(
                "cannot remove {} after its creation failed: {error:#}",
                workspace.name
            );
        }
    }
}
