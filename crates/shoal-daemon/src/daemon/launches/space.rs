//! A launch that starts its agent in a new space: the place its kind
//! decides, a chat's checkout, worktree or `root_dir`, or a workspace's
//! target and branch, then the one sequence every new space's start takes,
//! from the launch's name, agent, mode and issue. Where a workspace's
//! launch names a branch an existing workspace has, it starts that
//! workspace's own agent, and where it names no branch or issue it begins
//! as a chat.
use std::sync::Arc;

use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::launches::{Launch, LaunchSource, Launched};
use shoal_protocol::space_names::SpaceSelector;

use super::shape::Shape;
use crate::{
    config::modes::ModeName,
    daemon::{
        links::issue::OpenIssue,
        operations::starts::{Home, NewSpace, Opening, Preparing, SpaceStart, StartIssue},
        scope::{Caller, ScopeRefusal},
        workspace::{Manager, NewWorkspace},
    },
    forge::items::reference::ItemInput,
    model::{Blocker, SpaceKind},
    protocol::operations::LaunchRequest,
};

/// Where a launch's new space goes, with what only that place decides: the
/// issue it starts on, read, whether the launch starts an agent there, the
/// PR it links and what holds its agent.
pub(super) struct Placement {
    pub home: PlacedHome,
    pub issue: Option<StartIssue>,
    pub agent: bool,
    pub blockers: Vec<Blocker>,
}

/// The home of a launch's new space: a chat's, or the workspace it opens
/// in a repository.
pub(super) enum PlacedHome {
    Chat(Home<'static>),
    Workspace(RepositoryId, crate::start_plan::Opening),
}

/// What a launch of a new space starts as, once its place is known.
pub(super) enum Placed {
    New(Placement),
    /// The worker of the existing workspace that has the branch it names.
    Worker(Launch),
    /// A chat, which a launch of a workspace naming no branch or issue
    /// begins as, in its own shape.
    Chat(Launch, Shape),
}

impl Manager {
    /// Start the agent of `launch`, which `source` made, in the new space
    /// `shape` says, for `caller`.
    pub(super) async fn launch_space(
        self: &Arc<Self>,
        launch: Launch,
        shape: Shape,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        let placed = match shape.kind {
            SpaceKind::Chat => Placed::New(self.chat_placement(&launch, &shape, caller).await?),
            SpaceKind::Workspace => self.workspace_placement(&launch, &shape, caller).await?,
        };
        let placement = match placed {
            Placed::New(placement) => placement,
            Placed::Worker(worker) => {
                return self.launch_worker(worker, shape, source, caller).await;
            }
            Placed::Chat(chat, shape) => {
                return Box::pin(self.launch_space(chat, shape, source, caller)).await;
            }
        };
        ensure_placeable(&launch, placement.agent)?;
        let mut profile = None;
        let start = self.launch_start(launch, placement, (&shape, source), &mut profile);
        let mut start = start.await?;
        if let Some(caller) = caller {
            ScopeRefusal::of(self.confine_launch(caller, &mut start).await)?;
        }
        self.gates.admit()?;
        Ok(self.start_space(start, caller).await?.launched())
    }

    /// The start of the new space `launch` places as `placement` says, in
    /// `shape`, which `source` made, its workspace's Git profile kept in
    /// `profile`: named as it names it, below the parent it names, with
    /// the agent it asks for and its priority.
    async fn launch_start<'a>(
        &self,
        launch: Launch,
        Placement {
            home,
            issue,
            agent,
            mut blockers,
        }: Placement,
        (shape, source): (&Shape, LaunchSource),
        profile: &'a mut Option<String>,
    ) -> Result<SpaceStart<'a>> {
        let (home, pr) = placed_home(home, profile);
        // A launch that starts now waits for nothing, what it names included.
        if launch.now {
            blockers.clear();
        }
        let new = NewSpace {
            name: launch.name.clone(),
            parent: self.space_id(launch.parent.as_ref()).await?,
            priority: launch.priority,
        };
        let request = agent_request(&launch, &shape.mode, source);
        let preparing = Preparing {
            issue: None,
            pr,
            agent: agent.then(|| Box::new(request)),
            waiting: None,
            queue_entry: None,
            blockers,
            unfollowed: true,
            env: launch.setup_env,
        };
        Ok(SpaceStart {
            home,
            new,
            issue,
            preparing,
        })
    }

    /// Confine the scoped `caller`'s `start` to its own tree: below its own
    /// space unless it names a space below it, and for a workspace from a
    /// copy of one it reaches or reopening one below its own.
    async fn confine_launch(&self, caller: &Caller, start: &mut SpaceStart<'_>) -> Result<()> {
        let mut parent = (start.new.parent.as_ref()).map(SpaceSelector::id);
        match &mut start.home {
            Home::Opening(_, opening) => {
                (self.confine_workspace_start(caller, &mut parent, &mut opening.start)).await?;
            }
            _ => self.parent_below(caller, &mut parent).await?,
        }
        start.new.parent = self.space_id(parent.as_ref()).await?;
        Ok(())
    }

    /// The open issue `input` names in `repository`, which `launch` starts
    /// on, read as `caller` reads it. A launch that names its space needs
    /// no title from it but for a branch, so an issue it cannot read is
    /// taken by the URL its number has in the repository, which the
    /// agent's start reads again; one without even that is refused, since
    /// nothing could read it, as is a closed one.
    pub(super) async fn launch_issue(
        &self,
        launch: &Launch,
        input: &ItemInput,
        repository: &RepositoryId,
        caller: Option<&Caller>,
    ) -> Result<StartIssue> {
        let context = self.launch_read_context(repository, caller).await?;
        let error = match self.issue_details(&context, input, false, caller).await {
            Ok(issue) => {
                let (url, title, labels) = (issue.url, issue.title, issue.labels);
                return Ok(OpenIssue { url, title, labels }.into());
            }
            Err(error) if launch.name.is_some() && !closed(&error) => error,
            Err(error) => return Err(error),
        };
        crate::daemon::log!("issue {input} read without its title: {error:#}");
        let checkout = self.repository(repository.clone()).await?.path;
        let (_, url) = self.issue_forge(&checkout, input).await?;
        Ok(StartIssue {
            url,
            title: None,
            labels: Vec::new(),
        })
    }
}

/// The home `placed` names, a workspace's Git profile kept in `profile`,
/// and the PR a workspace it opens links once it is ready.
fn placed_home(placed: PlacedHome, profile: &mut Option<String>) -> (Home<'_>, Option<ItemInput>) {
    let (repository, opening) = match placed {
        PlacedHome::Chat(home) => return (home, None),
        PlacedHome::Workspace(repository, opening) => (repository, opening),
    };
    let crate::start_plan::Opening {
        start,
        base,
        path,
        git_profile,
        options,
        pr,
    } = opening;
    *profile = git_profile;
    let new = NewWorkspace {
        git_profile: profile.as_deref(),
        path,
        options,
        ..Default::default()
    };
    let opening = Opening {
        start,
        base,
        new,
        parent: None,
    };
    (Home::Opening(repository.into(), Box::new(opening)), pr)
}

/// The agent `launch` asks for in `mode`, which `source` made.
pub(super) fn agent_request(
    launch: &Launch,
    mode: &ModeName,
    source: LaunchSource,
) -> LaunchRequest {
    LaunchRequest {
        mode: Some(mode.clone()),
        narrowing: launch.permissions,
        model: launch.model.clone(),
        args: launch.task.clone(),
        prompt: launch.prompt.clone(),
        fork: launch.fork.clone(),
        env: launch.env.clone(),
        now: launch.now,
        here: launch.here,
        source: Some(source),
        ..LaunchRequest::new(launch.agent.clone())
    }
}

/// Whether `error` is the forge's refusal of a closed issue.
fn closed(error: &anyhow::Error) -> bool {
    (error.chain()).any(|cause| cause.is::<shoal_forge::items::issue::ClosedIssue>())
}

/// Refuse `launch` when it starts an agent and its caller's terminal could
/// place no agent in the background, as the launch says why.
fn ensure_placeable(launch: &Launch, agent: bool) -> Result<()> {
    match (agent, &launch.unplaceable) {
        (true, Some(refusal)) => anyhow::bail!("{refusal}"),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
