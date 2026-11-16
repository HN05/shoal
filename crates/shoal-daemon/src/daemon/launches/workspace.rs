//! A launch that creates or reopens a workspace: its target, what it starts
//! on, settled with the steps of [`crate::launches::workspace`] the CLI's
//! start takes too, its agent and issue, and the workspace it opens, the
//! existing one whose branch it names, or, without a branch or issue, a
//! chat whose agent makes it a workspace once it knows the work.
use shoal_protocol::launches::{Launch, LaunchSource, Launched, SpaceChoice};
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

use anyhow::Result;

use super::shape::Shape;
use crate::{
    daemon::{scope::Caller, workspace::Manager},
    launches::{self as fits, workspace::LaunchReads},
    model::{SpaceKind, Workspace},
    start_plan::Creation,
};

mod copy;
mod plan;
mod start;
mod target;

/// Shoal's state as a launch for `caller` reads it.
pub(super) struct ManagerReads<'a> {
    manager: &'a Manager,
    caller: Option<&'a Caller>,
}

impl LaunchReads for ManagerReads<'_> {
    async fn workspaces(&self) -> Result<Vec<Workspace>> {
        let root = self.manager.reach_root(self.caller).await?;
        self.manager.visible_workspaces(root.as_ref()).await
    }

    async fn check_opens(
        &self,
        repository: &crate::repository::Selector,
        existing: &str,
    ) -> Result<()> {
        let options = crate::daemon::workspace::NewWorktreeOptions::default();
        (self
            .manager
            .check_creation(repository, None, Some(existing), options))
        .await
    }
}

impl Manager {
    /// Start the agent of `launch`, which `source` made and `shape` starts
    /// in a new workspace, for `caller`.
    pub(super) async fn launch_workspace(
        self: &Arc<Self>,
        launch: Launch,
        shape: Shape,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        let reads = ManagerReads {
            manager: self,
            caller,
        };
        let copied = self.launch_copy(&launch, caller).await?;
        let mut creation = new_creation(&launch, copied.as_ref().map(|copy| &copy.creation));
        let copied_repository = match &copied {
            Some(copied) => Some(self.repository(copied.repository.clone()).await?),
            None => None,
        };
        let target =
            (self.workspace_target(&launch, &mut creation, copied_repository.as_ref(), &reads))
                .await?;
        let selector = (&target.repo.id).into();
        let settled =
            crate::launches::workspace::settle(&reads, (&selector, &target.repo), &mut creation);
        if let Some(workspace) = settled.await? {
            return self
                .launch_on_branch(launch, workspace, &creation, shape, source, caller)
                .await;
        }
        let blockers = (self.named_blockers(&target.repo.id, &launch.blockers, caller)).await?;
        if let Some(caller) = caller {
            let (given, repository) = (&launch.permissions, Some(&target.repo.id));
            (self.ensure_caller_holds(Some(&caller.space_id), given, repository)).await?;
        }
        let agent = self.planned_agent(&launch, &target, &creation).await?;
        let issue = self.planned_issue(&target, caller).await?;
        if issue.is_none() && creation.unsettled() {
            return self
                .launch_as_chat(launch, target.repo.id.to_string(), shape, source, caller)
                .await;
        }
        let creation = plan::issue_settled(&reads, &target.repo, creation, issue.as_ref()).await?;
        if let (Some(_), Some(refusal)) = (&agent.agent, &launch.unplaceable) {
            anyhow::bail!("{refusal}");
        }
        let planned = start::Planned {
            repo: target.repo,
            creation,
            agent,
            issue,
            blockers,
        };
        self.start_planned(launch, shape, planned, (source, &reads))
            .await
    }

    /// Start `launch`'s agent as the worker of `workspace`, which already has
    /// the branch it names, as `--in` starts it, refusing what only the new
    /// workspace of `creation` takes.
    async fn launch_on_branch(
        self: &Arc<Self>,
        launch: Launch,
        workspace: Workspace,
        creation: &Creation,
        shape: Shape,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        // The parent of a forked start without a copy is the workspace whose
        // agent it forks, which a start of an existing workspace's worker
        // takes too.
        let forks_parent = launch.fork.is_some() && creation.copy.is_none();
        let parent = launch.parent.is_some() && !forks_parent;
        let issue = launch.issue.is_some();
        if let Some(flag) = crate::launches::workspace::new_space_flag(creation, issue, parent) {
            let branch = workspace
                .branch
                .as_ref()
                .map_or("", |branch| branch.as_str());
            anyhow::bail!(
                "{flag} applies to a new workspace, and workspace {} already has branch {branch}; the start goes into it as --in does",
                workspace.number
            );
        }
        let worker = Launch {
            target: None,
            repository: None,
            branch: None,
            parent: None,
            in_workspace: Some(SpaceChoice::Space(SpaceSelector::id(&workspace.id))),
            ..launch
        };
        (self.launch_worker(worker, shape, source, caller))
            .await
            .map(|worker| Launched::Worker(Box::new(worker)))
    }

    /// Start `launch`'s agent, which names no branch, issue, PR or copy, in
    /// a chat in `repository` in its mode, whose agent makes it a workspace
    /// once it knows the work, refusing what only a new workspace takes.
    async fn launch_as_chat(
        &self,
        launch: Launch,
        repository: String,
        shape: Shape,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        if let Some(flag) = fits::workspace_flags(&launch).first() {
            anyhow::bail!(
                "{flag} applies to a new workspace's branch; name the branch with --branch, since a start without one begins as a chat"
            );
        }
        let chat = Launch {
            target: None,
            repository: Some(repository),
            chat: true,
            ..launch
        };
        let shape = Shape {
            kind: SpaceKind::Chat,
            repository: true,
            ..shape
        };
        (self.launch_in_chat(chat, shape, source, caller))
            .await
            .map(Launched::Chat)
    }
}

/// What `launch` creates as it names it, a copy of `copied` when it copies
/// one, before its target settles the rest.
fn new_creation(launch: &Launch, copied: Option<&crate::start_plan::CopyCreation>) -> Creation {
    Creation {
        path: launch.path.clone(),
        branch: launch.branch.clone(),
        base: launch.base.clone(),
        git_profile: launch.git_profile.clone(),
        options: crate::protocol::CreationOptions {
            name: launch.name.clone(),
            priority: launch.priority,
            skip_setup: launch.skip_setup,
            disposable: launch.disposable,
        },
        copy: copied.map(|copy| crate::start_plan::CopyCreation {
            source: copy.source.clone(),
            name: copy.name.clone(),
        }),
        ..Creation::default()
    }
}
