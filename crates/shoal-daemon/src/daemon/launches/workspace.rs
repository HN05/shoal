//! Where a launch's new workspace goes: its target, what it starts on,
//! settled with the steps of [`crate::launches::workspace`] the CLI's start
//! takes too, whether it starts an agent, its issue and what holds the
//! agent; or, where it names the branch of an existing workspace, that
//! workspace's own agent, and without a branch or issue a chat whose agent
//! makes it a workspace once it knows the work.
use shoal_protocol::launches::{Launch, SpaceChoice};
use shoal_protocol::space_names::SpaceSelector;

use anyhow::Result;

use super::{
    ManagerReads,
    shape::Shape,
    space::{Placed, PlacedHome, Placement},
};
use crate::{
    daemon::{manager::Manager, operations::starts::StartIssue, scope::Caller},
    forge::items::reference::ItemInput,
    launches as fits,
    model::{Blocker, Repository, SpaceKind, Workspace},
    start_plan::Creation,
};

mod copy;
mod plan;
mod target;

impl Manager {
    /// Where the new workspace `launch` starts in `shape` goes for
    /// `caller`, or what it starts instead, with an agent planned there
    /// unless it `creates` the workspace alone.
    pub(super) async fn workspace_placement(
        &self,
        launch: &Launch,
        (shape, creates): (&Shape, bool),
        caller: Option<&Caller>,
    ) -> Result<Placed> {
        let reads = ManagerReads::new(self, caller);
        let (mut creation, copied) = self.launch_creation(launch, caller).await?;
        let target =
            (self.workspace_target(launch, &mut creation, copied.as_ref(), &reads)).await?;
        let selector = (&target.repo.id).into();
        let settled =
            crate::launches::workspace::settle(&reads, (&selector, &target.repo), &mut creation);
        if let Some(workspace) = settled.await? {
            return Ok(Placed::Worker(on_branch(launch, workspace, &creation)?));
        }
        let mut blockers = (self.named_blockers(&target.repo.id, &launch.blockers, caller)).await?;
        let agent = !creates && self.planned_agent(launch, &target).await?;
        let issue = match &target.issue {
            Some(issue) => {
                let read = self.launch_issue(issue, (&target.repo.id, false), caller);
                Some(read.await?)
            }
            None => None,
        };
        if issue.is_none() && creation.unsettled() {
            return as_chat(launch, shape, &target.repo);
        }
        let creation = self.issue_creation(&reads, &target.repo, creation, issue.as_ref());
        let opening = creation.await?.opening()?;
        // An agent that starts now waits for nothing.
        if agent
            && !launch.now
            && let Some(issue) = &issue
        {
            blockers.extend(
                self.issue_forge_blockers(&target.repo, issue, caller)
                    .await?,
            );
        }
        Ok(Placed::New(Placement {
            home: PlacedHome::Workspace(target.repo.id, opening),
            issue,
            agent,
            blockers,
        }))
    }

    /// `creation` on the branch `issue`, read with its title, settles in
    /// `repo` when nothing else names one.
    async fn issue_creation(
        &self,
        reads: &ManagerReads<'_>,
        repo: &Repository,
        creation: Creation,
        issue: Option<&StartIssue>,
    ) -> Result<Creation> {
        let titled = issue.and_then(|issue| Some((&issue.url, issue.title.as_deref()?)));
        fits::workspace::issue_settled(reads, repo, creation, titled).await
    }

    /// The issues the forge records as blocking `issue` of `repo`.
    async fn issue_forge_blockers(
        &self,
        repo: &Repository,
        issue: &StartIssue,
        caller: Option<&Caller>,
    ) -> Result<Vec<Blocker>> {
        let input = ItemInput::Url(issue.url.clone());
        self.forge_blockers(&repo.id, &input, caller).await
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
}

/// The worker `launch` starts in `workspace`, which already has the branch
/// it names, as `--in` starts it, refusing what only the new workspace of
/// `creation` takes.
fn on_branch(launch: &Launch, workspace: Workspace, creation: &Creation) -> Result<Launch> {
    // The parent of a forked start without a copy is the workspace whose
    // agent it forks, which a start of an existing workspace's worker takes
    // too.
    let forks_parent = launch.fork.is_some() && creation.copy.is_none();
    let parent = launch.parent.is_some() && !forks_parent;
    let issue = launch.issue.is_some();
    if let Some(flag) = fits::workspace::new_space_flag(creation, issue, parent) {
        let branch = workspace.branch().map_or("", |branch| branch.as_str());
        anyhow::bail!(
            "{flag} applies to a new workspace, and workspace {} already has branch {branch}; the start goes into it as --in does",
            workspace.number
        );
    }
    Ok(Launch {
        target: None,
        repository: None,
        branch: None,
        parent: None,
        in_workspace: Some(SpaceChoice::Space(SpaceSelector::id(&workspace.id))),
        ..launch.clone()
    })
}

/// The chat `launch`, which names no branch, issue, PR or copy, begins as
/// in `repo`, in its mode of `shape`, whose agent makes it a workspace once
/// it knows the work, refusing what only a new workspace takes.
fn as_chat(launch: &Launch, shape: &Shape, repo: &Repository) -> Result<Placed> {
    if let Some(flag) = fits::workspace_flags(launch).first() {
        anyhow::bail!(
            "{flag} applies to a new workspace's branch; name the branch with --branch, since a start without one begins as a chat"
        );
    }
    let chat = Launch {
        target: None,
        repository: Some(repo.id.to_string()),
        chat: true,
        ..launch.clone()
    };
    let shape = Shape {
        kind: SpaceKind::Chat,
        mode: shape.mode.clone(),
        repository: true,
    };
    Ok(Placed::Chat(chat, shape))
}

#[cfg(test)]
mod tests;
