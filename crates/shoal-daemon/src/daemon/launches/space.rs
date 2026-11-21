//! A launch that starts its agent in a new space: the place its kind
//! decides, a chat's checkout, worktree or `root_dir`, or a workspace's
//! target and branch, then the one sequence every new space's start takes,
//! from the launch's name, agent, mode and issue. Where a workspace's
//! launch names a branch an existing workspace has, it starts that
//! workspace's own agent, and where it names no branch or issue it begins
//! as a chat.
use shoal_links::reads::checkouts::OpenIssue;
use shoal_protocol::progress::Blocker;
use std::sync::Arc;

use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::launches::{Launch, LaunchSource};
use shoal_protocol::protocol::Launched;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

use super::shape::Shape;
use crate::{
    config::modes::ModeName,
    daemon::{
        manager::Manager,
        operations::starts::{
            Home, NewSpace, Opening, Preparing, SpaceStart, StartIssue, StartedSpace,
        },
        scope::{Caller, ScopeRefusal},
        workspace::NewWorkspace,
    },
    forge::items::reference::ItemInput,
    model::SpaceKind,
    protocol::operations::LaunchRequest,
};

/// Where a launch's new space goes, with what only that place decides: the
/// issue it starts on, read, and what holds its agent.
pub(super) struct Placement {
    pub home: PlacedHome,
    pub issue: Option<StartIssue>,
    pub blockers: Vec<Blocker>,
}

/// The home of a launch's new space: a chat's, or the workspace it opens
/// in a repository.
pub(super) enum PlacedHome {
    Chat(Home<'static>),
    Workspace(RepositoryId, crate::start_plan::Opening),
}

/// What follows a placed space's recording: the agent it queues, if any,
/// and whether nothing follows its preparation, which may then wait for
/// disk space.
pub(super) struct Follows {
    pub agent: Option<LaunchRequest>,
    pub unfollowed: bool,
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
            SpaceKind::Workspace => {
                self.workspace_placement(&launch, (&shape, false), caller)
                    .await?
            }
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
        let agent = Some(agent_request(&launch, &shape.mode, source));
        let follows = Follows {
            agent,
            unfollowed: true,
        };
        let started = self.start_placed(launch, placement, follows, caller);
        Ok(started.await?.launched())
    }

    /// Record the new space `launch` places as `placement` says for
    /// `caller`, in its tree, and begin it with what `follows` it.
    pub(super) async fn start_placed(
        self: &Arc<Self>,
        launch: Launch,
        placement: Placement,
        follows: Follows,
        caller: Option<&Caller>,
    ) -> Result<StartedSpace> {
        let mut profile = None;
        let start = self.launch_start(launch, placement, follows, &mut profile);
        let mut start = start.await?;
        if let Some(caller) = caller {
            ScopeRefusal::of(self.confine_launch(caller, &mut start).await)?;
        }
        self.gates.admit()?;
        self.start_space(start, caller).await
    }

    /// The start of the new space `launch` places as `placement` says, its
    /// workspace's Git profile kept in `profile`: named as it names it,
    /// below the parent it names, with its priority, and what `follows` its
    /// recording.
    async fn launch_start<'a>(
        &self,
        launch: Launch,
        Placement {
            home,
            issue,
            mut blockers,
            ..
        }: Placement,
        Follows { agent, unfollowed }: Follows,
        profile: &'a mut Option<String>,
    ) -> Result<SpaceStart<'a>> {
        let (home, pr, name) = placed_home(home, launch.name.clone(), profile);
        // A launch that starts now waits for nothing, what it names included.
        if launch.now {
            blockers.clear();
        }
        let new = NewSpace {
            name,
            parent: self.parent_space_id(launch.parent.as_ref()).await?,
            priority: launch.priority,
        };
        let preparing = Preparing {
            issue: None,
            pr,
            agent: agent.map(Box::new),
            waiting: None,
            queue_entry: None,
            blockers,
            unfollowed,
            env: launch.setup_env,
        };
        Ok(SpaceStart {
            home,
            new,
            issue,
            preparing,
            unplaceable: launch.unplaceable,
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
        start.new.parent = self.parent_space_id(parent.as_ref()).await?;
        Ok(())
    }

    /// The open issue `input` names in `repository`, which a launch starts
    /// on, read as `caller` reads it. Where the place takes an unread issue
    /// `by_url`, as a chat its launch names does, which needs no title from
    /// it, an issue it cannot read is taken by the URL its number has in
    /// the repository, which the agent's start reads again; one without
    /// even that is refused, since nothing could read it, as is a closed
    /// one.
    pub(super) async fn launch_issue(
        &self,
        input: &ItemInput,
        (repository, by_url): (&RepositoryId, bool),
        caller: Option<&Caller>,
    ) -> Result<StartIssue> {
        let context = self.launch_read_context(repository, caller).await?;
        let error = match self
            .links
            .issue_details(self, &context, input, false, caller)
            .await
        {
            Ok(issue) => {
                let (url, title, labels) = (issue.url, issue.title, issue.labels);
                return Ok(OpenIssue { url, title, labels }.into());
            }
            Err(error) if by_url && !closed(&error) => error,
            Err(error) => return Err(error),
        };
        crate::daemon::log!("issue {input} read without its title: {error:#}");
        let checkout = self.repository(repository.clone()).await?.path;
        let (_, url) = self.links.issue_forge(self, &checkout, input).await?;
        Ok(StartIssue {
            url,
            title: None,
            labels: Vec::new(),
        })
    }
}

/// The home `placed` names, a workspace's Git profile kept in `profile`,
/// the PR a workspace it opens links once it is ready, and the name of
/// the space: the one the launch `named`, or for a workspace on a PR, the
/// title its placement read.
fn placed_home(
    placed: PlacedHome,
    named: Option<SpaceName>,
    profile: &mut Option<String>,
) -> (Home<'_>, Option<ItemInput>, Option<SpaceName>) {
    let (repository, opening) = match placed {
        PlacedHome::Chat(home) => return (home, None, named),
        PlacedHome::Workspace(repository, opening) => (repository, opening),
    };
    let crate::start_plan::Opening {
        start,
        base,
        path,
        git_profile,
        mut options,
        pr,
        files,
    } = opening;
    *profile = git_profile;
    let name = options.name.take();
    let new = NewWorkspace {
        git_profile: profile.as_deref(),
        path,
        options,
        files,
        ..Default::default()
    };
    let opening = Opening {
        start,
        base,
        new,
        parent: None,
    };
    (
        Home::Opening(repository.into(), Box::new(opening)),
        pr,
        name,
    )
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
        replace: launch.replace,
        source: Some(source),
        ..LaunchRequest::new(launch.agent.clone())
    }
}

/// Whether `error` is the forge's refusal of a closed issue.
fn closed(error: &anyhow::Error) -> bool {
    (error.chain()).any(|cause| cause.is::<shoal_forge::items::issue::ClosedIssue>())
}

#[cfg(test)]
mod tests;
