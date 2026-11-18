//! The agent a new space's start queues there and what else follows its
//! recording: the agent resolved under the settings where the space goes,
//! working on the start's issue, and refused before anything is recorded
//! where the caller may not give it what it asks for.
use std::collections::BTreeMap;

use anyhow::Result;
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId};
use shoal_protocol::space_names::SpaceSelector;

use super::{StartIssue, home::Home};
use crate::{
    agents::{check_queued_launch, resolve_launch},
    config::{Effective, modes::ModeName},
    daemon::{scope::Caller, workspace::Manager},
    forge::items::reference::ItemInput,
    model::{Blocker, Space, SpaceKind, Workspace},
    protocol::operations::{LaunchIssue, LaunchRequest, QueuedLaunch},
};

/// What follows a new space's recording, in the caller's environment
/// `env`, with its agent as a request names it or, by default, as the
/// daemon resolved it.
pub(crate) struct Preparing<L = QueuedLaunch> {
    /// The issue number or URL a request names, which its start reads
    /// open before the space is recorded.
    pub issue: Option<ItemInput>,
    pub pr: Option<ItemInput>,
    pub agent: Option<Box<L>>,
    /// The agent the caller starts itself once setup succeeds, which waits
    /// in the queue should setup fail.
    pub waiting: Option<Box<QueuedLaunch>>,
    /// The starting queue entry of the launch the start carries out, which
    /// the opened workspace takes before its preparation starts.
    pub queue_entry: Option<QueueEntryId>,
    /// What `agent` waits for besides the preparation, which waits for it
    /// too before it creates a new worktree, then creates it on the branch
    /// of the done blocker that holds the others' work.
    pub blockers: Vec<Blocker>,
    /// Whether nothing follows the preparation, which then also waits
    /// before it creates a new worktree while disk protection holds back
    /// new starts.
    pub unfollowed: bool,
    pub env: Option<BTreeMap<String, String>>,
}

impl Preparing<LaunchRequest> {
    /// This preparation with its agent resolved, and the agent the caller
    /// starts itself checked, under the `settings` they launch with in a
    /// space of the kind `place` says, read only when the start has either.
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
    /// Whether the preparation of `workspace` may wait before it creates
    /// the new worktree: for what its agent waits for, or, with nothing
    /// following it, for disk space.
    pub(super) fn may_wait(&self, workspace: &Workspace) -> bool {
        workspace.pending_worktree && (!self.blockers.is_empty() || self.unfollowed)
    }

    /// Queue its agent on the start's `issue`, which prompts it, with the
    /// task it names passed on beside it.
    pub(super) fn work_on(&mut self, issue: Option<&StartIssue>) {
        if let (Some(agent), Some(issue)) = (&mut self.agent, issue) {
            agent.issue.get_or_insert_with(|| LaunchIssue::from(issue));
        }
    }

    /// The mode its agent starts in.
    pub(super) fn mode(&self) -> Option<ModeName> {
        let agent = self.agent.as_ref()?;
        Some((agent.mode.clone()).unwrap_or(ModeName::DEFAULT))
    }
}

impl Manager {
    /// `preparing` with its agent resolved under the settings of a space at
    /// `home` in `repository` below `parent`.
    pub(super) async fn resolve_start_agent(
        &self,
        home: &Home<'_>,
        (repository, parent): (
            Option<&RepositoryId>,
            Option<&shoal_ids::space_ids::SpaceId>,
        ),
        preparing: Preparing<LaunchRequest>,
    ) -> Result<Preparing> {
        let place = (home.kind(), repository.is_some());
        (preparing.resolved(place, async || {
            self.home_settings(home, repository, parent).await
        }))
        .await
    }

    /// Refuse the scoped `caller` the agent `preparing` queues in a new
    /// space of `kind` in `repository`, before the start records anything,
    /// where it gives the agent permissions the caller does not hold, or
    /// starts it regardless of the limits beyond what the caller's `limits`
    /// permission allows.
    pub(super) async fn authorize_start_agent(
        &self,
        caller: Option<&Caller>,
        preparing: &Preparing,
        (kind, repository): (SpaceKind, Option<&RepositoryId>),
    ) -> Result<()> {
        let (Some(caller), Some(agent)) = (caller, &preparing.agent) else {
            return Ok(());
        };
        let (giver, given) = (Some(&caller.space_id), &agent.narrowing);
        (self.ensure_caller_holds(giver, given, repository)).await?;
        if !agent.now {
            return Ok(());
        }
        let start = format!("start the agent of a new {kind} now");
        (self.authorize_beyond_limits(caller, start, repository.cloned())).await
    }

    /// Refuse the scoped `caller` the agent `preparing` queues in the
    /// `space` a start recorded where it would start with a permission the
    /// caller does not hold: one it is given, or the value a key a reopened
    /// workspace's agent was given and this one leaves out falls back to.
    pub(super) async fn ensure_start_agent_held(
        &self,
        space: &Space,
        preparing: &Preparing,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let (Some(caller), Some(agent), Some(mode)) = (caller, &preparing.agent, preparing.mode())
        else {
            return Ok(());
        };
        let (giver, space) = (Some(&caller.space_id), SpaceSelector::id(&space.id));
        (self.ensure_agent_held(giver, &space, &mode, &agent.narrowing)).await
    }
}
