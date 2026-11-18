//! `Manager`, the daemon's shared state, with its configuration and the
//! workspace records it reads.
use crate::config_file::ConfigExt;
use crate::paths::StateFiles;
use shoal_ids::execution_ids::ExecutionId;
mod adoption;
pub(crate) mod agent_queue;
pub(crate) mod agent_state;
mod agent_turns;
pub(crate) mod agents;
mod branch_rename;
mod branches;
pub(crate) mod busy_cpu;
pub(crate) mod capacity;
mod checkpoints;
mod commits;
pub(crate) mod completion_cleanup;
mod config_state;
pub(crate) mod conflicts;
mod contributions;
mod converted;
mod coordinated;
mod copies;
mod creation;
mod daemon_exit;
pub(crate) mod demos;
mod diff;
mod disowned;
pub(crate) mod engagement;
pub(crate) mod executions;
mod fast_forward;
mod gates;
pub(crate) mod holds;
pub(in crate::daemon) mod identity;
mod imported_sessions;
mod kept_branches;
mod lifecycle;
pub(crate) use lifecycle::fail_interrupted_removals;
mod location;
pub(crate) mod lost_reports;
mod merge;
mod overload_recovery;
mod ownership;
pub(crate) mod paths;
pub(crate) mod places;
mod pr_gate;
pub(crate) mod proxy_retries;
mod refresh;
mod registry;
mod repo_configuration;
mod repo_move;
mod repo_removal;
mod reservation;
mod restore;
pub(crate) mod review;
pub(crate) mod runner;
mod sessions;
mod shutdown_stop;
mod signals;
mod space_removal;
pub(crate) mod spotlight;
mod stack;
mod status;
mod stopped_work;
pub(crate) mod subagents;
mod tree;
pub(crate) mod turn_retry;
pub(crate) mod upstream_limits;
mod usage;
mod xcode;

pub(crate) use agent_queue::Grant;
pub use agents::RestartWaits;
pub(crate) use agents::{Pause, Protection};
pub(crate) use branches::NewWorktreeOptions;
pub(crate) use contributions::holds::{
    ContributionHold, RESUME_WINDOW as CONTRIBUTION_RESUME_WINDOW,
};
pub(crate) use copies::CopyPlace;
pub(crate) use creation::{NewWorkspace, build_pending_in};
pub(crate) use daemon_exit::Ending;
pub(crate) use executions::{Completing, ExecutionRequest, ReattachedExecution, StartedExecution};
pub use executions::{ExecutionKind, StopRequest, UnprovenStop};
pub(crate) use gates::{
    ExitQuiescence, Exiting, GuardMode, HeldBack, PrGuard, ResourceGuard, ResourcesBusy,
};
pub(crate) use holds::place_approved as place_approved_hold;
pub(crate) use lifecycle::not_ready;
pub(crate) use merge::RecordChange;
pub(crate) use ownership::VerifiedWorktree;
pub(crate) use shutdown_stop::StoppedSpaces;
pub(crate) use stack::record_base_changed;
pub use stopped_work::PermissionTargets;

pub(crate) use tree::{
    Source, TrackedAgents, children, deciders, delegated, ensure_parent, parent, receiving_paths,
    set_parent, spaces_above, subtree,
};

use crate::{
    config::Config,
    daemon::{
        spaces::status::Records,
        store::{self, Store},
    },
    model::{Execution, Inspection, Repository, Workspace},
    paths::Paths,
};
use anyhow::Result;
use rusqlite::OptionalExtension;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use std::{collections::HashSet, sync::Arc};
use tokio::sync::OwnedMutexGuard;

pub struct Manager {
    pub store: Store,
    pub(crate) session_hosts: Vec<Arc<dyn crate::sessions::Host>>,
    /// Held while the owner of a recorded place is read and its place
    /// closed, or its ownership moves to another workspace.
    pub(crate) place_ownership: tokio::sync::Mutex<()>,
    pub gates: gates::Gates,
    pub(crate) waits: crate::daemon::links::wait::ActiveWaits,
    /// The reads of items for watches, and those watches started from.
    pub(crate) watch_starts: crate::daemon::links::wait::WatchStarts,
    pub(super) cleanup_health: super::cleanup::CleanupHealth,
    /// How the loop that puts idle agents to sleep is doing.
    pub(super) idle_sleep_health: super::cleanup::CleanupHealth,
    /// The spaces automatic cleanup removed since Shoal's log recorded it.
    pub(crate) cleanup_removals: super::cleanup::Removed,
    pub(crate) syncs: super::checkout_sync::Syncs,
    pub(crate) triggers: super::triggers::Triggers,
    /// The spaces whose PRs' progress sections wait for an update.
    pub(crate) publications: super::progress::Publications,
    /// The spaces whose issues wait for the agent account to be settled.
    pub(crate) agent_settles: super::links::issue::agent_settles::AgentSettles,
    pub config_state: config_state::ConfigState,
    pub paths: Paths,
    /// Arrival order of queued resource and simulator requests.
    pub(crate) queues: super::queue::Queues,
    pub executions: executions::Executions,
    pub overload_recovery: overload_recovery::OverloadRecovery,
    /// The busy CPU the overload monitor reads for job admission.
    pub(crate) busy_cpu: busy_cpu::BusyCpu,
    pub agent_queue: agent_queue::AgentQueue,
    /// The contributions a running `shoal contribute` keeps from cleanup.
    pub(crate) contribution_holds: contributions::holds::ContributionHolds,
    pub(crate) operations: Arc<crate::daemon::operations::Operations>,
    /// The scope tokens approved commands take their permission's actions
    /// with.
    pub(crate) grants: crate::daemon::permissions::Grants,
    /// Held through a stop before a shutdown; true once one finished, until
    /// a system shutdown is cancelled, so later ones leave the work alone.
    pub(crate) shutdown_stopped: tokio::sync::Mutex<bool>,
    pub(crate) signals: signals::Signals,
    /// The time runs of agent messages settle by.
    pub(crate) settle_clock: crate::daemon::agent_messages::SettleClock,
    pub(crate) usage: usage::Usage,
    pub(crate) leftovers: crate::daemon::leftovers::Leftovers,
    /// Held while a converted space's agent is handed over to the new place,
    /// so a stop and the relocation never both hand it over.
    pub(crate) conversion_handovers: crate::daemon::conversions::Handovers,
    /// Held while an agent is admitted to a workspace, until it is tracked,
    /// and while a workspace's conversion into a chat is recorded, so no
    /// agent starts in a workspace that becomes a chat.
    pub(crate) agent_admissions: tokio::sync::Mutex<()>,
    /// The spaces a removal holds, whose agent no registration runs again.
    pub(crate) space_removals: crate::daemon::spaces::Removing,
    /// The completed spaces whose completion a removal of a space below
    /// them goes by, which keep the workspaces below them meanwhile.
    pub(crate) completed_owners: crate::daemon::spaces::Removing,
    pub(crate) seeds: super::seed::Seeds,
    pub(crate) forge_memories: crate::daemon::links::ForgeMemories,
    pub(crate) reviews: crate::daemon::reviews::Reviews,
    pub(crate) gate: crate::daemon::gate::Gatekeeper,
    pub(crate) runners: crate::daemon::runner::Runners,
    /// One-shot agent runs whose processes may still be running.
    pub(crate) oneshot_runs: crate::daemon::oneshot::Runs,
    /// The retry each proxied execution's proxy waits to make.
    proxy_retries:
        std::sync::Mutex<std::collections::HashMap<ExecutionId, proxy_retries::ProxyRetry>>,
    /// The limit each proxied execution's proxy found its upstream under,
    /// with the time it stands until.
    upstream_limits:
        std::sync::Mutex<std::collections::HashMap<ExecutionId, upstream_limits::UpstreamLimit>>,
    /// The config files of worktrees the current format rejects, which the
    /// daemon ignores.
    rejected_configs: repo_configuration::RejectedConfigs,
    /// When each space's agent state was last reported.
    report_times: Arc<lost_reports::ReportTimes>,
    /// Directories whose file access stalls, shared with startup's status answer.
    pub(crate) file_access: Arc<crate::daemon::file_access::FileAccess>,
    /// The hub role, or this machine's membership of a hub.
    pub(crate) hub: crate::daemon::hub::Hub,
}

impl Manager {
    pub async fn open(paths: Paths) -> Result<Arc<Self>> {
        paths.prepare()?;
        let config = Config::load(&paths)?;
        super::forge_hosts::learn_forge_pins(&Default::default(), &config.machine.forges);
        Ok(Arc::new(Self {
            config_state: config_state::ConfigState::new(config),
            waits: Default::default(),
            watch_starts: Default::default(),
            cleanup_health: Default::default(),
            idle_sleep_health: super::cleanup::CleanupHealth::of("Idle sleep"),
            cleanup_removals: Default::default(),
            syncs: Default::default(),
            triggers: Default::default(),
            publications: Default::default(),
            agent_settles: Default::default(),
            store: Store::open(paths.database()).await?,
            // Tests inject hosts and never discover or close the user's
            // sessions.
            session_hosts: if crate::daemon::fake_workers::faked() {
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
            busy_cpu: Default::default(),
            agent_queue: Default::default(),
            contribution_holds: Default::default(),
            operations: Default::default(),
            grants: Default::default(),
            shutdown_stopped: Default::default(),
            signals: Default::default(),
            settle_clock: Default::default(),
            usage: Default::default(),
            leftovers: Default::default(),
            conversion_handovers: Default::default(),
            agent_admissions: Default::default(),
            space_removals: Default::default(),
            completed_owners: Default::default(),
            seeds: Default::default(),
            forge_memories: Default::default(),
            reviews: Default::default(),
            gate: Default::default(),
            runners: Default::default(),
            oneshot_runs: Default::default(),
            proxy_retries: Default::default(),
            upstream_limits: Default::default(),
            rejected_configs: Default::default(),
            report_times: Default::default(),
            file_access: Default::default(),
            hub: Default::default(),
        }))
    }

    /// The global config as last loaded; see [`config_state::ConfigState::current`].
    pub fn config(&self) -> Arc<Config> {
        self.config_state.current()
    }

    /// Whether this daemon acts on what the global config sets up for the
    /// machine, such as its triggers, listeners and hub role: a development
    /// build leaves those of the user's own config to the installed Shoal,
    /// whose daemon acts on them.
    pub(crate) fn acts_on_global_config(&self) -> bool {
        let config = crate::config::Config::path(&self.paths);
        crate::config_file::refuse_user_config(&config).is_ok()
    }

    /// Make `config` the one later operations read. Changed overload
    /// settings withdraw recovery readiness before anything can read them, so
    /// a waiting agent needs a fresh healthy interval under the new thresholds.
    pub fn publish_config(&self, config: Config) {
        if config.machine.overload != self.config().machine.overload {
            self.overload_recovery.reset();
        }
        super::forge_hosts::learn_forge_pins(&self.config().machine.forges, &config.machine.forges);
        self.config_state.publish(config);
        self.triggers.wake();
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
            .run(move |db| workspaces_within(db, root.as_ref()))
            .await
    }

    pub async fn workspace_ids(&self) -> Result<HashSet<SpaceId>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare("SELECT id FROM spaces WHERE kind='workspace'")?
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

    /// Whether the workspace has any recorded execution, in any state.
    pub(super) async fn workspace_has_executions(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| store::exists(db, "SELECT 1 FROM executions WHERE space_id=?1", [id]))
            .await
    }

    /// Read recorded executions in every state for a known workspace ID.
    pub async fn workspace_executions(&self, id: &SpaceId) -> Result<Vec<Execution>> {
        let id = id.to_owned();
        self.store.run(move |db| store::executions(db, &id)).await
    }

    /// Count recorded executions in every state without decoding their details.
    pub(super) async fn workspace_execution_count(&self, id: &SpaceId) -> Result<usize> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let count: i64 = db.query_row(
                    "SELECT COUNT(*) FROM executions WHERE space_id=?1",
                    [id],
                    |row| row.get(0),
                )?;
                Ok(usize::try_from(count)?)
            })
            .await
    }

    /// The workspace with this ID, unless it was removed, read in one store
    /// operation, so a removal meanwhile reads as none.
    pub async fn find_workspace(&self, id: &SpaceId) -> Result<Option<Workspace>> {
        let id = id.to_owned();
        self.store.run(move |db| recorded_workspace(db, &id)).await
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
        let workspace = self.workspace(selector).await?;
        let simulators = self.list_simulators(Some(&workspace.id)).await?;
        let Records {
            issues,
            pr_cleanup,
            completion,
        } = self.space_records(&workspace.id).await?;
        let mut inspection = self
            .store
            .run(move |db| {
                Ok(Inspection {
                    issues,
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

    /// When the space `id` was last recorded active: a workspace without
    /// any starts its idle period now, while a chat records no such time.
    pub(crate) async fn active_at(&self, id: &SpaceId) -> Result<Option<crate::time::UnixTime>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET active_at=unixepoch() WHERE id=?1 AND active_at IS NULL",
                    [&id],
                )?;
                Ok(db
                    .query_row(
                        "SELECT active_at FROM workspaces WHERE id=?1",
                        [&id],
                        |row| row.get(0),
                    )
                    .optional()?)
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
/// `root` and every workspace below it, or every workspace without `root`,
/// by name, each with its details.
pub(crate) fn workspaces_within(
    db: &rusqlite::Connection,
    root: Option<&SpaceId>,
) -> Result<Vec<Workspace>> {
    let mut workspaces = db
        .prepare(&tree::subtree_query(store::WORKSPACE_COLUMNS))?
        .query_map([root], store::workspace)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for workspace in &mut workspaces {
        load_details(db, workspace)?;
    }
    Ok(workspaces)
}

/// The workspace `id` with its details, while it exists.
fn recorded_workspace(db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<Workspace>> {
    let query = format!(
        "SELECT {} FROM spaces JOIN workspaces USING(id) WHERE id=?1",
        store::WORKSPACE_COLUMNS
    );
    let workspace = db.query_row(&query, [id], store::workspace).optional()?;
    let Some(mut workspace) = workspace else {
        return Ok(None);
    };
    load_details(db, &mut workspace)?;
    Ok(Some(workspace))
}

fn load_details(db: &rusqlite::Connection, workspace: &mut Workspace) -> Result<()> {
    workspace.holds = holds::list(db, &workspace.id)?;
    workspace.review = review::list(db, &workspace.id)?;
    workspace.latest_review = crate::daemon::reviews::store::latest_status(db, &workspace.id)?;
    workspace.agent_state = agent_state::current(db, &workspace.id)?;
    workspace.progress = crate::daemon::progress::current(db, &workspace.id)?;
    workspace.sleep = crate::daemon::idle_wakes::sleep::current(db, &workspace.id)?;
    workspace.wake_at = crate::daemon::wake_timers::due_at(db, &workspace.id)?;
    workspace.asleep = agent_queue::asleep::current(db, &workspace.id)?;
    stack::load(db, workspace)?;
    copies::load(db, workspace)?;
    tree::load(db, workspace)?;
    workspace.links = store::space_links(db, &workspace.id)?;
    workspace.running = store::exists(
        db,
        "SELECT 1 FROM executions WHERE space_id=?1 AND state=?2",
        rusqlite::params![workspace.id, crate::state::ExecutionState::Running],
    )?;
    workspace.done = store::exists(
        db,
        "SELECT 1 FROM space_completion WHERE space_id=?1",
        [&workspace.id],
    )?;
    workspace.pending_worktree = store::exists(
        db,
        "SELECT 1 FROM pending_worktrees WHERE workspace_id=?1",
        [&workspace.id],
    )?;
    workspace.preparation_waits =
        crate::daemon::operations::starts::preparation_waits(db, &workspace.id)?;
    Ok(())
}

#[cfg(test)]
mod tests;
