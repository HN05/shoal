//! What a machine reports to the hub of each of its spaces.
use crate::log;
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use tokio::time::Instant;

use anyhow::Result;
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    agent::Agent,
    approvals::{ApprovalRequest, DecisionStatus},
    config::permissions::Permissions,
    hub::directory::{RepositoryOrigin, SpaceSummary},
    list_status::ListStatus,
    model::{ByKind, QueueEntry, Repository, RunningAgent, Space},
    questions::Question,
};

use crate::{Hub, daemon::Daemon};

/// How long a repository's origin stays as reports read it before they
/// read it again, so a changed origin reaches the hub without a Git read
/// on every change.
const ORIGIN_FRESH: Duration = Duration::from_secs(5 * 60);

/// The origins of this machine's repositories as reports last read them,
/// each with when.
#[derive(Default)]
pub(super) struct Origins(HashMap<RepositoryId, (Option<String>, Instant)>);

impl Origins {
    /// `repository` as other machines tell it: its origin, read again once
    /// [`ORIGIN_FRESH`] passed, and the name it has now.
    async fn of(&mut self, repository: &Repository) -> RepositoryOrigin {
        let origin = match self.0.get(&repository.id) {
            Some((origin, read)) if read.elapsed() < ORIGIN_FRESH => origin.clone(),
            _ => {
                // A checkout whose origin cannot be read matches no other
                // machine's.
                let read = crate::repository::identity(&repository.source).await;
                let origin = read.unwrap_or_default();
                let entry = (origin.clone(), Instant::now());
                self.0.insert(repository.id.clone(), entry);
                origin
            }
        };
        RepositoryOrigin {
            origin,
            name: repository_name(repository),
        }
    }
}

impl Hub {
    /// A summary of each of this machine's spaces, by ID.
    pub(super) async fn space_summaries(
        &self,
        daemon: &dyn Daemon,
        origins: &mut Origins,
    ) -> Result<HashMap<SpaceId, SpaceSummary>> {
        let spaces = daemon.listed_spaces(None).await?;
        let stopped: HashSet<SpaceId> = (daemon.saved_summaries(None).await?.into_iter())
            .filter(|saved| saved.unfinished)
            .map(|saved| saved.space)
            .collect();
        let questions = daemon.open_questions().await?;
        let mut approvals = daemon.approval_requests(None).await?;
        approvals.retain(|request| request.status == DecisionStatus::Pending);
        let running = running_agents(daemon.running_agents().await?);
        let records = self.records.clone();
        let parents = (self.store).run(move |db| records.parent_ids(db)).await?;
        let with_agents = daemon.with_agents().await?;
        let around = Around {
            stopped,
            queued: daemon.visible_queue().await?,
            questions,
            approvals,
            parents,
            agents: with_agents,
            running,
        };
        let repositories = daemon.repositories().await?;
        let mut summaries = HashMap::new();
        for space in spaces {
            let repository = (space.repository_id.as_ref())
                .and_then(|id| repositories.iter().find(|repository| &repository.id == id));
            let repository = match repository {
                Some(repository) => Some(origins.of(repository).await),
                None => None,
            };
            let permissions = self.reported_permissions(daemon, &space).await;
            let chat_modes = daemon.chat_modes(&space).await.unwrap_or_default();
            let mut summary = summary(space, repository, &around);
            summary.permissions = permissions;
            summary.chat_modes = chat_modes;
            summaries.insert(summary.id.clone(), summary);
        }
        Ok(summaries)
    }
}

impl Hub {
    /// The effective permissions of `space`, which narrow those of the
    /// spaces below it on other machines; none while they cannot be read.
    async fn reported_permissions(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
    ) -> Option<Permissions> {
        let repository = space.repository_id.as_ref();
        match daemon.space_permissions(&space.id, repository).await {
            Ok(permissions) => Some(permissions),
            Err(error) => {
                log!(
                    "hub: could not report the permissions of {}: {error:#}",
                    space.name
                );
                None
            }
        }
    }
}

/// What a summary reads besides a space's record: the spaces whose agent's
/// stop saved work to restore, the agents queued to start, the open
/// questions and pending approval requests of every space, the space above
/// each, which may run on another machine, the spaces with an agent, and
/// the agent that runs in each.
struct Around {
    stopped: HashSet<SpaceId>,
    queued: Vec<QueueEntry>,
    questions: Vec<Question>,
    approvals: Vec<ApprovalRequest>,
    parents: HashMap<SpaceId, SpaceId>,
    agents: HashSet<SpaceId>,
    running: HashMap<SpaceId, Agent>,
}

/// The agent each space runs among `agents`, by name, where Shoal did not
/// stop it.
fn running_agents(agents: Vec<RunningAgent>) -> HashMap<SpaceId, Agent> {
    (agents.into_iter())
        .filter(|running| running.stopped.is_none())
        .filter_map(|running| Some((running.space_id, running.agent?)))
        .collect()
}

/// The summary of `space`, of `repository`, with what `around` holds of it.
fn summary(space: Space, repository: Option<RepositoryOrigin>, around: &Around) -> SpaceSummary {
    let kind = space.kind();
    let (id, number, name) = (space.id.clone(), space.number, space.name.clone());
    let parent = around.parents.get(&space.id).cloned();
    let agent = around.agents.contains(&space.id);
    let running = around.running.get(&space.id).cloned();
    let (mode, progress) = (space.mode.clone(), space.progress.clone());
    let agent_state = space.agent_state.map(|status| status.state);
    let stopped = around.stopped.contains(&id);
    let status = match space.by_kind() {
        ByKind::Workspace(workspace) => {
            ListStatus::of_queued_workspace(&workspace, stopped, &around.queued)
        }
        ByKind::Chat(chat) => ListStatus::of_chat(&chat),
    };
    let questions = (around.questions.iter())
        .filter(|question| question.space_id == id)
        .cloned()
        .collect();
    let approvals = (around.approvals.iter())
        .filter(|request| request.space_id == id)
        .cloned()
        .collect();
    SpaceSummary {
        id,
        number,
        name,
        kind,
        repository,
        parent,
        mode,
        status,
        agent_state,
        agent,
        running,
        permissions: None,
        chat_modes: Vec::new(),
        progress,
        questions,
        approvals,
    }
}

/// The name a repository shows: its own, or else its directory's.
fn repository_name(repository: &Repository) -> String {
    repository.name.clone().unwrap_or_else(|| {
        let directory = repository.workspaces_dir.file_name();
        directory.map_or_else(
            || repository.id.to_string(),
            |name| name.to_string_lossy().into(),
        )
    })
}

#[cfg(test)]
mod tests;
