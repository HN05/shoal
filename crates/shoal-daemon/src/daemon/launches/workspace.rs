//! A launch that creates or reopens a workspace: its target, what it starts
//! on, settled with the steps of [`crate::launches::workspace`] the CLI's
//! start takes too, its agent and issue, and the workspace it opens, the
//! existing one whose branch it names, or, without a branch or issue, a
//! chat whose agent makes it a workspace once it knows the work.
use shoal_protocol::launches::{Launch, LaunchSource, Launched, SpaceChoice};
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

use anyhow::Result;

use super::{ManagerReads, shape::Shape};
use crate::{
    daemon::{scope::Caller, workspace::Manager},
    launches as fits,
    model::{Repository, SpaceKind, Workspace},
    start_plan::Creation,
};

mod copy;
mod plan;
mod start;
mod target;

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
        let reads = ManagerReads::new(self, caller);
        let (mut creation, copied) = self.launch_creation(&launch, caller).await?;
        let target =
            (self.workspace_target(&launch, &mut creation, copied.as_ref(), &reads)).await?;
        let selector = (&target.repo.id).into();
        let settled =
            crate::launches::workspace::settle(&reads, (&selector, &target.repo), &mut creation);
        if let Some(workspace) = settled.await? {
            return self
                .launch_on_branch(launch, workspace, &creation, shape, source, caller)
                .await;
        }
        let blockers = (self.named_blockers(&target.repo.id, &launch.blockers, caller)).await?;
        self.ensure_launch_held(&launch, &target.repo, caller)
            .await?;
        let agent = self.planned_agent(&launch, &target, &creation).await?;
        let issue = self.planned_issue(&target, caller).await?;
        if issue.is_none() && creation.unsettled() {
            return self
                .launch_as_chat(launch, target.repo.id.to_string(), shape, source, caller)
                .await;
        }
        let issue_named = (issue.as_ref()).map(|issue| (&issue.url, issue.title.as_str()));
        let creation =
            fits::workspace::issue_settled(&reads, &target.repo, creation, issue_named).await?;
        ensure_placeable(&launch, &agent)?;
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

    /// What `launch` creates as it names it, a copy of the workspace it
    /// copies, if any, whose repository comes with it.
    async fn launch_creation(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<(Creation, Option<Repository>)> {
        let Some(copied) = self.launch_copy(launch, caller).await? else {
            return Ok((fits::workspace::launch_creation(launch, None), None));
        };
        let repository = self.repository(copied.repository.clone()).await?;
        Ok((
            fits::workspace::launch_creation(launch, Some(copied.creation)),
            Some(repository),
        ))
    }

    /// Refuse the permissions `launch` gives its agent in `repo` where the
    /// scoped `caller` does not hold them.
    async fn ensure_launch_held(
        &self,
        launch: &Launch,
        repo: &Repository,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let Some(caller) = caller else {
            return Ok(());
        };
        let (given, repository) = (&launch.permissions, Some(&repo.id));
        (self.ensure_caller_holds(Some(&caller.space_id), given, repository)).await
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
            let branch = workspace.branch().map_or("", |branch| branch.as_str());
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
        self.launch_worker(worker, shape, source, caller).await
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
        self.launch_new_chat(chat, shape, source, caller).await
    }
}

/// Refuse `launch` when it starts `agent` and its caller's terminal could
/// place no agent in the background, as the launch says why.
fn ensure_placeable(launch: &Launch, agent: &plan::PlannedAgent) -> Result<()> {
    match (&agent.agent, &launch.unplaceable) {
        (Some(_), Some(refusal)) => anyhow::bail!("{refusal}"),
        _ => Ok(()),
    }
}
