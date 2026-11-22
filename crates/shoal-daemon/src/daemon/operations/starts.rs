//! Starting a new space through one sequence, whatever its kind and
//! whoever starts it, a launch, `StartWorkspace` or `StartChild`: its agent
//! resolved under the settings where it goes, its name, the checks before
//! anything is recorded, the space recorded at its home, which decides its
//! kind, linked to the issue it starts on, its agent queued, and the
//! preparation of a workspace, the only step that depends on the kind,
//! which creates a new branch's worktree first, unless it waits for the
//! work its agent waits for or for disk space, and whose failure the
//! start's coordinator or the user hears of.
mod agent;
mod failures;
mod home;
mod link;
mod name;
mod preparation;
mod queue;
mod waiting;

pub(crate) use agent::Preparing;
pub(crate) use home::{Child, Home, Opening, Recorded};
pub(crate) use link::StartIssue;
pub(crate) use waiting::{note_stopped, preparation_waits};

use std::sync::Arc;

use anyhow::{Context as _, Result, bail, ensure};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    protocol::{Launched, Started},
    space_names::{SpaceName, SpaceSelector},
};
use shoal_schedule::priority::Priority;

use crate::{
    daemon::{
        manager::Manager,
        scope::{Caller, ScopeRefusal},
    },
    forge::items::reference::ItemInput,
    model::{QueueEntry, Space},
    protocol::operations::{LaunchRequest, StartedWorkspace},
    repository::Selector,
};

/// A new space as a start describes it: where it goes, what it is named
/// and below which space, the issue it starts on, and what follows its
/// recording.
pub(crate) struct SpaceStart<'a> {
    pub home: Home<'a>,
    pub new: NewSpace,
    pub issue: Option<StartIssue>,
    pub preparing: Preparing<LaunchRequest>,
    /// Why the caller's terminal could place no agent in the background,
    /// which refuses a start once its agent resolves.
    pub unplaceable: Option<String>,
}

/// What a start names a new space, below which space it goes, and the
/// priority it asks for it.
#[derive(Default)]
pub(crate) struct NewSpace {
    pub name: Option<SpaceName>,
    pub parent: Option<SpaceId>,
    pub priority: Option<Priority>,
}

/// A space a start recorded and began: as the start left it, with its
/// agent's queue entry, what linking its issue warned of, and how its agent
/// starts.
pub(crate) struct StartedSpace {
    pub space: Space,
    pub entry: Option<Box<QueueEntry>>,
    pub warnings: Vec<String>,
    pub started: Started,
}

impl StartedSpace {
    /// The launch of the agent this start queued.
    pub(crate) fn launched(self) -> Launched {
        Launched {
            space: Box::new(self.space),
            entry: self.entry,
            warnings: self.warnings,
            start: self.started,
            machine: None,
            placement: None,
        }
    }

    /// The workspace this start opened, with the operation preparing it.
    pub(crate) fn into_workspace(self) -> Result<StartedWorkspace> {
        let Started::Preparing { operation, reused } = self.started else {
            bail!("the start began no workspace");
        };
        let workspace =
            (self.space.into_workspace().ok()).context("the start began no workspace")?;
        Ok(StartedWorkspace {
            operation,
            workspace: Box::new(workspace),
            reused,
            entry: self.entry,
            warnings: self.warnings,
        })
    }
}

impl Manager {
    /// Record the space `start` describes for `caller` and begin it.
    pub(crate) async fn start_space(
        self: &Arc<Self>,
        start: SpaceStart<'_>,
        caller: Option<&Caller>,
    ) -> Result<StartedSpace> {
        let SpaceStart {
            home,
            mut new,
            issue,
            preparing,
            unplaceable,
        } = start;
        let repository = self.home_repository(&home).await?;
        let place = (repository.as_ref(), new.parent.as_ref());
        let mut preparing = self.resolve_start_agent(&home, place, preparing).await?;
        if let (Some(_), Some(refusal)) = (&preparing.agent, unplaceable) {
            bail!("{refusal}");
        }
        preparing.work_on(issue.as_ref());
        let named = new.name.take();
        new.name = (self.start_name(&home, named, issue.as_ref(), &preparing)).await?;
        let kind = home.kind();
        (self.check_start((kind, repository.as_ref()), &preparing, caller)).await?;
        let asked = new.priority;
        let given = self.given_priority(caller, asked).await?;
        let agent = preparing.agent.as_deref();
        let recorded = self.record_space(home, new, agent, caller).await?;
        // A reopened workspace keeps its priority unless one was asked for.
        let priority = if recorded.reused { asked } else { given };
        self.give_priority(&recorded.space.id, priority).await?;
        self.begin_recorded(recorded, issue, preparing, caller)
            .await
    }

    /// Refuse a start of a space of `kind` in `repository` before anything
    /// is recorded where `caller` may not queue its agent as `preparing`
    /// says, or the work the agent waits for cannot hold it.
    async fn check_start(
        &self,
        (kind, repository): (crate::model::SpaceKind, Option<&RepositoryId>),
        preparing: &Preparing,
        caller: Option<&Caller>,
    ) -> Result<()> {
        ensure!(
            preparing.blockers.is_empty() || preparing.agent.is_some(),
            "--add-blocker queues an agent; name one with --agent"
        );
        (self.authorize_start_agent(caller, preparing, (kind, repository))).await?;
        match repository {
            Some(repository) if !preparing.blockers.is_empty() => {
                let blockers = preparing.blockers.clone();
                self.agents
                    .check_blockers(self, &repository.into(), blockers)
                    .await
            }
            _ => Ok(()),
        }
    }

    /// Begin the `recorded` space for `caller`: link it to `issue`, queue
    /// the agent `preparing` names there, which waits for its preparation,
    /// and start preparing a workspace.
    pub(crate) async fn begin_recorded(
        self: &Arc<Self>,
        recorded: Recorded,
        issue: Option<StartIssue>,
        mut preparing: Preparing,
        caller: Option<&Caller>,
    ) -> Result<StartedSpace> {
        (self.ensure_start_agent_held(&recorded.space, &preparing, caller)).await?;
        self.ensure_start_may_replace(&recorded.space, &preparing, caller)
            .await?;
        let warnings = self.link_start_issue(&recorded, issue, caller).await?;
        let admission = self.queue_space_agent(&recorded, &mut preparing).await?;
        let (space, started) = self
            .prepare_space(&recorded, preparing, admission.as_ref(), caller)
            .await?;
        let entry = admission.and_then(|admission| admission.entry);
        Ok(StartedSpace {
            space,
            entry: entry.map(Box::new),
            warnings,
            started,
        })
    }

    /// Refuse the scoped `caller` the agent `preparing` names where it
    /// replaces the stopped agent of the reopened `space` and the caller may
    /// not discard that agent.
    async fn ensure_start_may_replace(
        &self,
        space: &Space,
        preparing: &Preparing,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let replaces = (preparing.agent.as_ref()).is_some_and(|agent| agent.replace);
        let Some(caller) = caller.filter(|_| replaces) else {
            return Ok(());
        };
        let mut selector = SpaceSelector::id(&space.id);
        let allowed = self
            .agents
            .ensure_may_replace(self, caller, &mut selector, true)
            .await;
        ScopeRefusal::of(allowed)
    }

    /// Open the workspace `opening` names in `repository`, a new one or
    /// the one on its branch or PR head, and start preparing it.
    pub(crate) async fn start_workspace(
        self: &Arc<Self>,
        repository: Selector,
        mut opening: Opening<'_>,
        preparing: Preparing<LaunchRequest>,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        let issue = (self.read_start_issue(&repository, preparing.issue.as_ref())).await?;
        let new = NewSpace {
            name: opening.new.options.name.take(),
            parent: opening.parent.take(),
            priority: opening.new.options.priority,
        };
        let home = Home::Opening(repository, Box::new(opening));
        let start = SpaceStart {
            home,
            new,
            issue,
            preparing,
            unplaceable: None,
        };
        self.start_space(start, caller).await?.into_workspace()
    }

    /// Create the child `child` of the space `parent`, a workspace or a
    /// chat, on its base or a copy, leaving out what its options say, and
    /// start preparing it.
    pub(crate) async fn start_child(
        self: &Arc<Self>,
        parent: &SpaceSelector,
        mut child: Child,
        preparing: Preparing<LaunchRequest>,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        let (parent, repo) = self.child_parent(parent, child.copy.as_ref()).await?;
        let repository = (&repo.id).into();
        let issue = (self.read_start_issue(&repository, preparing.issue.as_ref())).await?;
        let new = NewSpace {
            name: child.options.name.take(),
            parent: Some(parent.id),
            priority: child.options.priority,
        };
        let start = SpaceStart {
            home: Home::Child(repo.id, child),
            new,
            issue,
            preparing,
            unplaceable: None,
        };
        self.start_space(start, caller).await?.into_workspace()
    }

    /// The open issue `issue` names in `repository`, which a request's
    /// start begins on.
    async fn read_start_issue(
        &self,
        repository: &Selector,
        issue: Option<&ItemInput>,
    ) -> Result<Option<StartIssue>> {
        let Some(issue) = issue else {
            return Ok(None);
        };
        let checkout = self.repositories.repository(repository.clone()).await?.path;
        Ok(Some(
            self.links
                .read_open_issue(self, &checkout, issue)
                .await?
                .into(),
        ))
    }
}

#[cfg(test)]
mod tests;
