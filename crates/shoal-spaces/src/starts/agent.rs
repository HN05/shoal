//! The agent a new space's start queues and what else follows its
//! recording. The caller may not give the agent what its own space does not
//! hold.
use crate::Spaces;
use crate::daemon::Daemon;
use shoal_protocol::progress::Blocker;
use std::collections::BTreeMap;

use anyhow::Result;
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId};
use shoal_protocol::space_names::SpaceSelector;

use super::{StartIssue, home::Home};
use crate::{
    agents::{check_queued_launch, resolve_launch},
    config::{Effective, modes::ModeName},
    forge::items::reference::ItemInput,
    model::{Space, SpaceKind, Workspace},
    protocol::operations::{LaunchIssue, LaunchRequest, QueuedLaunch},
    scope::Caller,
};

/// In the caller's environment `env`. The agent is as a request names it or,
/// by default, as the daemon resolved it.
pub struct Preparing<L = QueuedLaunch> {
    /// The start reads it open before the space is recorded.
    pub issue: Option<ItemInput>,
    pub pr: Option<ItemInput>,
    pub agent: Option<Box<L>>,
    /// The agent the caller starts itself once setup succeeds. It waits in
    /// the queue should setup fail.
    pub waiting: Option<Box<QueuedLaunch>>,
    /// The opened workspace takes it before its preparation starts.
    pub queue_entry: Option<QueueEntryId>,
    /// What `agent` waits for besides the preparation. The preparation waits
    /// for it too before it creates a new worktree, then creates it on the
    /// branch of the done blocker that holds the others' work.
    pub blockers: Vec<Blocker>,
    /// Nothing follows the preparation, which then also waits before it
    /// creates a new worktree while disk protection holds back new starts.
    pub unfollowed: bool,
    pub env: Option<BTreeMap<String, String>>,
}

impl Preparing<LaunchRequest> {
    /// Resolves under the `settings` a space of the kind `place` says
    /// launches with, read only when the start has an agent.
    pub(super) async fn resolved(
        self,
        place: (SpaceKind, bool),
        settings: impl AsyncFnOnce() -> Result<Effective>,
    ) -> Result<Preparing> {
        let Self {
            issue,
            pr,
            agent,
            waiting,
            queue_entry,
            blockers,
            unfollowed,
            env,
        } = self;
        let agent = match (agent, &waiting) {
            (None, None) => None,
            (agent, waiting) => {
                let settings = settings().await?;
                if let Some(waiting) = waiting {
                    check_queued_launch(waiting, &settings, place)?;
                }
                let resolved = agent.map(|agent| resolve_launch(*agent, &settings, place));
                resolved.transpose()?.map(Box::new)
            }
        };
        Ok(Preparing {
            issue,
            pr,
            agent,
            waiting,
            queue_entry,
            blockers,
            unfollowed,
            env,
        })
    }
}

impl Preparing {
    /// For what its agent waits for, or, with nothing following it, for disk
    /// space.
    pub(super) fn may_wait(&self, workspace: &Workspace) -> bool {
        workspace.checkout.is_pending() && (!self.blockers.is_empty() || self.unfollowed)
    }

    /// The issue prompts it, with the task it names passed on beside it.
    pub(super) fn work_on(&mut self, issue: Option<&StartIssue>) {
        if let (Some(agent), Some(issue)) = (&mut self.agent, issue) {
            agent.issue.get_or_insert_with(|| LaunchIssue::from(issue));
        }
    }

    pub(super) fn mode(&self) -> Option<ModeName> {
        let agent = self.agent.as_ref()?;
        Some((agent.mode.clone()).unwrap_or(ModeName::DEFAULT))
    }
}

impl Spaces {
    pub(super) async fn resolve_start_agent(
        &self,
        daemon: &dyn Daemon,
        home: &Home<'_>,
        (repository, parent): (
            Option<&RepositoryId>,
            Option<&shoal_ids::space_ids::SpaceId>,
        ),
        preparing: Preparing<LaunchRequest>,
    ) -> Result<Preparing> {
        let place = (home.kind(), repository.is_some());
        (preparing.resolved(place, async || {
            self.home_settings(daemon, home, repository, parent).await
        }))
        .await
    }

    /// Runs before the start records anything. Refuses permissions the caller
    /// does not hold, and starting regardless of the limits beyond what its
    /// `limits` permission allows.
    pub(super) async fn authorize_start_agent(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        preparing: &Preparing,
        (kind, repository): (SpaceKind, Option<&RepositoryId>),
    ) -> Result<()> {
        let (Some(caller), Some(agent)) = (caller, &preparing.agent) else {
            return Ok(());
        };
        let (giver, given) = (Some(caller), &agent.narrowing);
        (daemon
            .coordination()
            .ensure_caller_holds(daemon, giver, given, repository))
        .await?;
        if !agent.now {
            return Ok(());
        }
        let start = format!("start the agent of a new {kind} now");
        (daemon
            .coordination()
            .authorize_beyond_limits(daemon, caller, start, repository.cloned()))
        .await
    }

    /// Refuses a permission the caller does not hold: one the agent is
    /// given, or the value a key falls back to that a reopened workspace's
    /// agent was given and this one leaves out.
    pub(super) async fn ensure_start_agent_held(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        preparing: &Preparing,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let (Some(caller), Some(agent), Some(mode)) = (caller, &preparing.agent, preparing.mode())
        else {
            return Ok(());
        };
        let (giver, space) = (Some(caller), SpaceSelector::id(&space.id));
        (daemon
            .coordination()
            .ensure_agent_held(daemon, giver, &space, &mode, &agent.narrowing))
        .await
    }
}
