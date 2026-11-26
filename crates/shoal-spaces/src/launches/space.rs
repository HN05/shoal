//! A launch that starts its agent in a new space: the place its kind
//! decides, then the one sequence every new space's start takes. See
//! `docs/design/spaces.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::daemon::OpenIssue;
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
    forge::items::reference::ItemInput,
    model::SpaceKind,
    protocol::operations::LaunchRequest,
    {
        scope::{Caller, ScopeRefusal},
        starts::{Home, NewSpace, Opening, Preparing, SpaceStart, StartIssue, StartedSpace},
        workspace::NewWorkspace,
    },
};
use shoal_core::launches;

/// The issue it starts on, read, and what holds its agent.
pub(super) struct Placement {
    pub home: PlacedHome,
    pub issue: Option<StartIssue>,
    pub blockers: Vec<Blocker>,
}

pub(super) enum PlacedHome {
    Chat(Home<'static>),
    Workspace(RepositoryId, crate::start_plan::Opening),
}

/// The agent it queues, if any, and whether nothing follows its
/// preparation, which may then wait for disk space.
pub(super) struct Follows {
    pub agent: Option<LaunchRequest>,
    pub unfollowed: bool,
}

pub(super) enum Placed {
    New(Placement),
    /// The worker of the existing workspace that has the branch it names.
    Worker(Launch),
    /// A launch of a workspace that names no branch or issue begins as a
    /// chat in its own shape.
    Chat(Launch, Shape),
}

impl Spaces {
    pub(super) async fn launch_space(
        &self,
        daemon: &Arc<dyn Daemon>,
        launch: Launch,
        shape: Shape,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        let placed = match shape.kind {
            SpaceKind::Chat => Placed::New(
                self.chat_placement(&**daemon, &launch, &shape, caller)
                    .await?,
            ),
            SpaceKind::Workspace => {
                self.workspace_placement(&**daemon, &launch, (&shape, false), caller)
                    .await?
            }
        };
        let placement = match placed {
            Placed::New(placement) => placement,
            Placed::Worker(worker) => {
                return self
                    .launch_worker(&**daemon, worker, shape, source, caller)
                    .await;
            }
            Placed::Chat(chat, shape) => {
                return Box::pin(self.launch_space(daemon, chat, shape, source, caller)).await;
            }
        };
        let agent = Some(agent_request(&launch, &shape.mode, source));
        let follows = Follows {
            agent,
            unfollowed: true,
        };
        let started = self.start_placed(daemon, launch, placement, follows, caller);
        Ok(started.await?.launched())
    }

    pub(super) async fn start_placed(
        &self,
        daemon: &Arc<dyn Daemon>,
        launch: Launch,
        placement: Placement,
        follows: Follows,
        caller: Option<&Caller>,
    ) -> Result<StartedSpace> {
        let mut profile = None;
        let start = self.launch_start(&**daemon, launch, placement, follows, &mut profile);
        let mut start = start.await?;
        if let Some(caller) = caller {
            ScopeRefusal::of(self.confine_launch(&**daemon, caller, &mut start).await)?;
        }
        self.gates.admit()?;
        self.start_space(daemon, start, caller).await
    }

    /// Keeps the workspace's Git profile in `profile`.
    async fn launch_start<'a>(
        &self,
        daemon: &dyn Daemon,
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
        // A launch that starts now waits for nothing, what it names
        // included.
        if launch.now {
            blockers.clear();
        }
        let new = NewSpace {
            name,
            parent: self.parent_space_id(daemon, launch.parent.as_ref()).await?,
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

    /// Below its own space unless it names a space below it. A workspace
    /// copies only one it reaches, and reopens only one below its own.
    async fn confine_launch(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        start: &mut SpaceStart<'_>,
    ) -> Result<()> {
        let mut parent = (start.new.parent.as_ref()).map(SpaceSelector::id);
        match &mut start.home {
            Home::Opening(_, opening) => {
                (daemon.coordination().confine_workspace_start(
                    daemon,
                    caller,
                    &mut parent,
                    &mut opening.start,
                ))
                .await?;
            }
            _ => {
                daemon
                    .coordination()
                    .parent_below(daemon, caller, &mut parent)
                    .await?
            }
        }
        start.new.parent = self.parent_space_id(daemon, parent.as_ref()).await?;
        Ok(())
    }

    /// Read as `caller` reads it. Where the place takes an unread issue
    /// `by_url`, as a chat does, which needs no title, an issue it cannot
    /// read is taken by the URL its number has in the repository, which the
    /// agent's start reads again. One without even that is refused, since
    /// nothing could read it, and so is a closed one.
    pub(super) async fn launch_issue(
        &self,
        daemon: &dyn Daemon,
        input: &ItemInput,
        (repository, by_url): (&RepositoryId, bool),
        caller: Option<&Caller>,
    ) -> Result<StartIssue> {
        let context = self.launch_read_context(repository, caller).await?;
        let error = match daemon.issue_details(&context, input, false, caller).await {
            Ok(issue) => {
                let (url, title, labels) = (issue.url, issue.title, issue.labels);
                return Ok(OpenIssue { url, title, labels }.into());
            }
            Err(error) if by_url && !closed(&error) => error,
            Err(error) => return Err(error),
        };
        crate::log!("issue {input} read without its title: {error:#}");
        let checkout = daemon
            .repository(crate::repository::Selector::from(repository.clone()))
            .await?
            .path;
        let (_, url) = daemon.issue_forge(&checkout, input).await?;
        Ok(StartIssue {
            url,
            title: None,
            labels: Vec::new(),
        })
    }
}

/// Keeps a workspace's Git profile in `profile`. Also returns the PR a
/// workspace links once it is ready, and the name of the space: the one the
/// launch `named`, or for a workspace on a PR, the title its placement
/// read.
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
        run: launches::run::settled_run(launch).cloned(),
        fork: launch.fork.clone(),
        env: launch.env.clone(),
        now: launch.now,
        here: launch.here,
        replace: launch.replace,
        source: Some(source),
        ..LaunchRequest::new(launch.agent.clone())
    }
}

pub fn closed(error: &anyhow::Error) -> bool {
    (error.chain()).any(|cause| cause.is::<shoal_forge::items::issue::ClosedIssue>())
}

#[cfg(test)]
mod tests;
