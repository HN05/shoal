//! `Manager`, the daemon's shared state, with its configuration and the
//! workspace records it reads.
use crate::client::execution_ids::ExecutionId;
use crate::config_file::ConfigExt;
mod adoption;
pub(crate) mod agent_queue;
pub(crate) mod agent_state;
mod agent_turns;
pub(crate) mod agents;
mod branches;
pub(crate) mod capacity;
mod checkpoints;
mod commits;
mod config_state;
pub(crate) mod conflicts;
mod contributions;
mod coordinated;
mod copies;
mod creation;
mod daemon_exit;
mod demos;
mod diff;
pub(crate) mod done;
mod environment;
pub(crate) mod executions;
mod fast_forward;
mod gates;
mod holds;
pub(super) mod identity;
mod imported_sessions;
mod kept_branches;
mod lifecycle;
mod location;
mod merge;
mod overload_recovery;
mod ownership;
pub(crate) mod paths;
pub(crate) mod places;
mod pr_gate;
pub(crate) mod proxy_retries;
mod registry;
mod rename;
mod repo_configuration;
mod repo_move;
mod repo_removal;
mod reservation;
mod restore;
pub(crate) mod review;
pub(crate) mod runner;
mod sessions;
mod shared_scans;
mod shutdown_stop;
mod signals;
pub(crate) mod spotlight;
mod stack;
mod status;
mod stopped_work;
mod title;
mod tree;
pub(crate) mod turn_retry;
mod usage;
mod xcode;

pub(crate) use agent_queue::Grant;
pub use agents::RestartWaits;
pub(crate) use agents::{Outcome, Pause, Protection, Restart, Restarts, restarted};
pub(crate) use branches::NewWorktreeOptions;
pub(crate) use copies::CopyPlace;
pub(crate) use creation::NewWorkspace;
pub(crate) use daemon_exit::Ending;
pub(crate) use executions::{Completing, ExecutionRequest, ReattachedExecution, StartedExecution};
pub use executions::{ExecutionKind, StopRequest, UnprovenStop};
pub(crate) use gates::{GuardMode, PrGuard, ResourceGuard};
pub(crate) use holds::place_approved as place_approved_hold;
pub(crate) use imported_sessions::kept_session_sources;
pub(crate) use merge::RecordChange;
pub(crate) use ownership::VerifiedWorktree;
pub(crate) use stack::record_base_changed;

pub(crate) use tree::{
    Source, TrackedAgents, children, deciders, delegated, ensure_parent, parent, receiving_paths,
    set_parent, spaces_above,
};

use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::refusal::{Reason, Refusal};
use crate::{
    config::Config,
    daemon::store::{self, Store},
    model::{Execution, Inspection, Repository, Workspace},
    paths::Paths,
};
use anyhow::Result;
use futures_util::StreamExt as _;
use rusqlite::OptionalExtension;
use std::{collections::HashSet, fs, sync::Arc};
use tokio::sync::OwnedMutexGuard;

/// Review checks a listing runs at once, each reading HEAD with one Git
/// process: enough to overlap their waits without one listing taking a
/// process per workspace.
const REVIEW_CHECK_CONCURRENCY: usize = 8;

pub struct Manager {
    pub store: Store,
    pub(crate) session_hosts: Vec<Arc<dyn crate::sessions::Host>>,
    /// Held while the owner of a recorded place is read and its place
    /// closed, or its ownership moves to another workspace.
    pub(crate) place_ownership: tokio::sync::Mutex<()>,
    pub(crate) gates: gates::Gates,
    pub(crate) item_waits: crate::daemon::links::wait::ActiveWaits,
    pub(crate) chat_waits: crate::daemon::links::wait::ActiveWaits,
    pub(super) cleanup_health: super::cleanup::CleanupHealth,
    /// The spaces automatic cleanup removed since Shoal's log recorded it.
    pub(crate) cleanup_removals: super::cleanup::Removed,
    pub(crate) syncs: super::checkout_sync::Syncs,
    /// The spaces whose PRs' progress sections wait for an update.
    pub(crate) publications: super::progress::Publications,
    pub(crate) config_state: config_state::ConfigState,
    pub(crate) paths: Paths,
    /// Arrival order of queued resource and simulator requests.
    pub(crate) queues: super::queue::Queues,
    pub(crate) executions: executions::Executions,
    pub(crate) overload_recovery: overload_recovery::OverloadRecovery,
    pub(crate) agent_queue: agent_queue::AgentQueue,
    pub(crate) operations: Arc<crate::daemon::operations::Operations>,
    /// Held through a stop before a shutdown; true once one finished, until
    /// a system shutdown is cancelled, so later ones leave the work alone.
    pub(crate) shutdown_stopped: tokio::sync::Mutex<bool>,
    pub(crate) signals: signals::Signals,
    /// The time runs of agent messages settle by.
    pub(crate) settle_clock: crate::daemon::agent_messages::SettleClock,
    /// Process scans shared by the checks for agents' running work.
    pub(crate) work_scans: shared_scans::SharedScans,
    pub(crate) usage: usage::Usage,
    pub(crate) leftovers: crate::daemon::leftovers::Leftovers,
    /// Held shared from reading the tracked processes until those found
    /// untracked are stopped, and exclusively to hand a chat to another
    /// process, so a chat's new process is never found untracked: its
    /// launcher waits for the handoff and keeps it a tracked descendant.
    pub(crate) chat_handoffs: tokio::sync::RwLock<()>,
    /// Held while chats are saved and their processes stopped, and to
    /// restore or remove a stopped chat, so neither overlaps a stop.
    pub(crate) chat_saves: tokio::sync::Mutex<()>,
    pub(crate) chat_stops: crate::daemon::chats::Stopping,
    /// The restarts of chats' agents.
    pub(crate) chat_agents: crate::daemon::chats::ChatAgents,
    /// The chats a removal holds, which no restore runs again.
    pub(crate) chat_removals: crate::daemon::chats::Removing,
    /// The chats whose workspaces a removal under their completion takes,
    /// which keep the workspaces below them meanwhile.
    pub(crate) chat_owners: crate::daemon::chats::Removing,
    pub(crate) seeds: super::seed::Seeds,
    pub(crate) forge_memories: crate::daemon::links::ForgeMemories,
    pub(crate) reviews: crate::daemon::reviews::Reviews,
    pub(crate) runners: crate::daemon::runner::Runners,
    /// One-shot agent runs whose processes may still be running.
    pub(crate) oneshot_runs: crate::daemon::oneshot::Runs,
    /// The retry each proxied execution's proxy waits to make.
    proxy_retries:
        std::sync::Mutex<std::collections::HashMap<ExecutionId, proxy_retries::ProxyRetry>>,
    /// The config files of worktrees the current format rejects, which the
    /// daemon ignores.
    rejected_configs: repo_configuration::RejectedConfigs,
    /// Directories whose file access stalls, shared with startup's status answer.
    pub(crate) file_access: Arc<crate::daemon::file_access::FileAccess>,
}

impl Manager {
    pub async fn open(paths: Paths) -> Result<Arc<Self>> {
        paths.prepare()?;
        // Avoid inheriting personal Worktrunk hooks and layout preferences.
        fs::write(paths.worktrunk_config(), "# Managed by Shoal.\n")?;
        let config = Config::load(&paths)?;
        super::forge_hosts::learn_forge_pins(&Default::default(), &config.machine.forges);
        Ok(Arc::new(Self {
            config_state: config_state::ConfigState::new(config),
            item_waits: Default::default(),
            chat_waits: Default::default(),
            cleanup_health: Default::default(),
            cleanup_removals: Default::default(),
            syncs: Default::default(),
            publications: Default::default(),
            store: Store::open(paths.database()).await?,
            // Unit tests inject hosts and never discover or close the
            // user's sessions.
            session_hosts: if cfg!(test) {
                Vec::new()
            } else {
                crate::sessions::hosts()
            },
            place_ownership: Default::default(),
            gates: Default::default(),
            paths,
            queues: Default::default(),
            executions: Default::default(),
            overload_recovery: Default::default(),
            agent_queue: Default::default(),
            operations: Default::default(),
            shutdown_stopped: Default::default(),
            signals: Default::default(),
            settle_clock: Default::default(),
            work_scans: Default::default(),
            usage: Default::default(),
            leftovers: Default::default(),
            chat_handoffs: Default::default(),
            chat_saves: Default::default(),
            chat_stops: Default::default(),
            chat_agents: Default::default(),
            chat_removals: Default::default(),
            chat_owners: Default::default(),
            seeds: Default::default(),
            forge_memories: Default::default(),
            reviews: Default::default(),
            runners: Default::default(),
            oneshot_runs: Default::default(),
            proxy_retries: Default::default(),
            rejected_configs: Default::default(),
            file_access: Default::default(),
        }))
    }

    /// The global config as last loaded; see [`config_state::ConfigState::current`].
    pub fn config(&self) -> Arc<Config> {
        self.config_state.current()
    }

    /// Make `config` the one later operations read. Changed overload
    /// settings withdraw recovery readiness before anything can read them, so
    /// a waiting agent needs a fresh healthy interval under the new thresholds.
    pub(crate) fn publish_config(&self, config: Config) {
        if config.machine.overload != self.config().machine.overload {
            self.overload_recovery.reset();
        }
        super::forge_hosts::learn_forge_pins(&self.config().machine.forges, &config.machine.forges);
        self.config_state.publish(config);
    }

    /// Replace the global config with the file's current contents for later
    /// operations. An invalid file leaves the running config in place.
    pub async fn reload_config(&self) -> Result<()> {
        let _reload = self.config_state.lock_reload().await;
        let paths = self.paths.clone();
        let config = tokio::task::spawn_blocking(move || Config::load_settled(&paths)).await??;
        self.publish_config(config);
        Ok(())
    }

    pub(crate) async fn lock_repository(
        &self,
        selector: impl Into<crate::repository::Selector>,
    ) -> Result<(Repository, OwnedMutexGuard<()>)> {
        let repo = self.repository(selector).await?;
        let guard = self.gates.lock_git(&repo.id).await;
        // Removal may have completed or failed while this request waited.
        let repo = self.repository(&repo.id).await?;
        self.ensure_repository_available(&repo.id).await?;
        Ok((repo, guard))
    }

    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        self.list_workspaces_within(None).await
    }

    /// `root` and every workspace below it, or every workspace without
    /// `root`, by name. Only the selected workspaces load their details.
    pub async fn list_workspaces_within(&self, root: Option<&SpaceId>) -> Result<Vec<Workspace>> {
        let root = root.cloned();
        self.store
            .run(move |db| {
                let mut workspaces = db
                    .prepare(&tree::subtree_query(store::WORKSPACE_COLUMNS))?
                    .query_map([root], store::workspace)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for workspace in &mut workspaces {
                    load_details(db, workspace)?;
                }
                Ok(workspaces)
            })
            .await
    }

    pub async fn workspace_ids(&self) -> Result<HashSet<SpaceId>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare("SELECT id FROM workspaces")?
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// What the workspace's processes use, from the latest sample.
    pub(crate) fn workspace_usage(&self, id: &SpaceId) -> Option<crate::model::WorkspaceUsage> {
        let now = tokio::time::Instant::now();
        self.usage.with_workspaces(|usages| usages.get(id, now))
    }

    /// The workspaces a caller lists, with review marks checked against
    /// HEAD and their coordinators' turn states. A workspace process sees its
    /// own subtree with its own workspace first, since scoped commands
    /// without a target act on the first one; a chat in a workspace's
    /// worktree also sees that workspace, last.
    pub async fn visible_workspaces(&self, owner: Option<&SpaceId>) -> Result<Vec<Workspace>> {
        let mut workspaces = self.list_workspaces_within(owner).await?;
        if let Some(owner) = owner {
            if let Some(home) = self.chat_home(owner).await? {
                workspaces.push(self.workspace(&SpaceSelector::id(&home)).await?);
            }
            workspaces.sort_by_key(|w| w.id != *owner);
        }
        let deadline = review::check_deadline();
        futures_util::stream::iter(&mut workspaces)
            .for_each_concurrent(REVIEW_CHECK_CONCURRENCY, |workspace| {
                self.annotate_review(workspace, deadline)
            })
            .await;
        for workspace in &mut workspaces {
            workspace.usage = self.workspace_usage(&workspace.id);
        }
        self.load_coordinator_states(&mut workspaces).await?;
        self.attach_jobs(&mut workspaces).await?;
        Ok(workspaces)
    }

    /// Whether the workspace has any recorded execution, in any state.
    pub(super) async fn workspace_has_executions(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                store::exists(db, "SELECT 1 FROM executions WHERE workspace_id=?1", [id])
            })
            .await
    }

    /// Read recorded executions in every state for a known workspace ID.
    pub(super) async fn workspace_executions(&self, id: &SpaceId) -> Result<Vec<Execution>> {
        let id = id.to_owned();
        self.store.run(move |db| store::executions(db, &id)).await
    }

    /// Count recorded executions in every state without decoding their details.
    pub(super) async fn workspace_execution_count(&self, id: &SpaceId) -> Result<usize> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let count: i64 = db.query_row(
                    "SELECT COUNT(*) FROM executions WHERE workspace_id=?1",
                    [id],
                    |row| row.get(0),
                )?;
                Ok(usize::try_from(count)?)
            })
            .await
    }

    /// The workspace with the ID `id`, which is never read as a name.
    pub async fn workspace_by_id(&self, id: &SpaceId) -> Result<Workspace> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let mut workspace = db
                    .query_row(
                        &format!(
                            "SELECT {} FROM workspaces WHERE id=?1",
                            store::WORKSPACE_COLUMNS
                        ),
                        [&id],
                        store::workspace,
                    )
                    .optional()?
                    .ok_or_else(|| {
                        Refusal::new(Reason::NotFound, format!("unknown workspace: {id}"))
                    })?;
                load_details(db, &mut workspace)?;
                Ok(workspace)
            })
            .await
    }

    /// The workspace with this ID, unless it was removed.
    pub async fn find_workspace(&self, id: &SpaceId) -> Result<Option<Workspace>> {
        let owned = id.to_owned();
        let exists = self
            .store
            .run(move |db| store::exists(db, "SELECT 1 FROM workspaces WHERE id=?1", [owned]))
            .await?;
        Ok(match exists {
            true => Some(self.workspace_by_id(id).await?),
            false => None,
        })
    }

    /// Resolve an optional selector to a workspace ID filter.
    pub async fn workspace_filter(
        &self,
        selector: Option<&SpaceSelector>,
    ) -> Result<Option<SpaceId>> {
        match selector {
            Some(selector) => Ok(Some(self.workspace(selector).await?.id)),
            None => Ok(None),
        }
    }

    pub async fn inspect_workspace(&self, selector: &SpaceSelector) -> Result<Inspection> {
        let mut workspace = self.workspace(selector).await?;
        self.annotate_review(&mut workspace, review::check_deadline())
            .await;
        let simulators = self.list_simulators(Some(&workspace.id)).await?;
        let issue = self.issue_registration(&workspace.id).await?;
        let pr_cleanup = self.pr_registration(&workspace.id).await?;
        let completion = self.completion(&workspace.id).await?;
        let mut inspection = self
            .store
            .run(move |db| {
                Ok(Inspection {
                    issue,
                    completion,
                    pr_cleanup,
                    executions: store::executions(db, &workspace.id)?,
                    places: places::owned(db, Some(&workspace.id))?
                        .into_iter()
                        .map(|(_, place)| place)
                        .collect(),
                    ports: store::ports(db, Some(&workspace.id))?,
                    resources: crate::daemon::resources::leases(db, Some(&workspace.id))?,
                    simulators,
                    workspace,
                })
            })
            .await?;
        self.annotate_stops(&mut inspection.executions).await;
        Ok(inspection)
    }

    /// Record activity so automatic cleanup restarts its idle timer, and its
    /// time for idle periods that must survive a restart.
    pub async fn touch(&self, id: &SpaceId) {
        self.usage.record_activity(id);
        let id = id.to_owned();
        let recorded = self
            .store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET active_at=unixepoch() WHERE id=?1",
                    [id],
                )?;
                Ok(())
            })
            .await;
        if let Err(error) = recorded {
            crate::daemon::log!("activity time not recorded: {error:#}");
        }
    }

    /// When the workspace was last recorded active; one without any starts
    /// its idle period now.
    pub(crate) async fn active_at(&self, id: &SpaceId) -> Result<crate::time::UnixTime> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET active_at=unixepoch() WHERE id=?1 AND active_at IS NULL",
                    [&id],
                )?;
                Ok(db.query_row(
                    "SELECT active_at FROM workspaces WHERE id=?1",
                    [&id],
                    |row| row.get(0),
                )?)
            })
            .await
    }

    /// Drop in-memory bookkeeping for a workspace that no longer exists.
    pub(crate) async fn forget_workspace(&self, id: &SpaceId) {
        self.usage.forget_workspace(id);
        self.rejected_configs.forget(id);
        self.executions.forget_workspace(id).await;
    }
}

/// Fill in what a workspace record reads from its related tables.
fn load_details(db: &rusqlite::Connection, workspace: &mut Workspace) -> Result<()> {
    workspace.holds = holds::list(db, &workspace.id)?;
    workspace.review = review::list(db, &workspace.id)?;
    workspace.latest_review = crate::daemon::reviews::store::latest_status(db, &workspace.id)?;
    workspace.agent_state = agent_state::current(db, &workspace.id)?;
    workspace.progress = crate::daemon::progress::current(db, &workspace.id)?;
    stack::load(db, workspace)?;
    copies::load(db, workspace)?;
    tree::load(db, workspace)?;
    workspace.links = store::space_links(db, &workspace.id)?;
    workspace.running = store::exists(
        db,
        "SELECT 1 FROM executions WHERE workspace_id=?1 AND state=?2",
        rusqlite::params![workspace.id, crate::state::ExecutionState::Running],
    )?;
    workspace.done = store::exists(
        db,
        "SELECT 1 FROM workspace_completion WHERE workspace_id=?1",
        [&workspace.id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
