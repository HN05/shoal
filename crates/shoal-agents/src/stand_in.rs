//! A stand-in for the rest of the daemon, which the agents' tests build the
//! service over.
mod fixture;
mod records;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use shoal_core::{
    config::{Effective, repo::ConfigLayers},
    hooks::HookKind,
    process::load::WorkLimit,
};
use shoal_daemon_base::{
    alerts::{About, AlertKind},
    callers::Caller,
    jobs::{JobCost, JobStop},
};
use shoal_forge_types::items::{link::LinkKind, reference::ItemInput};
use shoal_ids::{
    execution_ids::ExecutionId, queue_entry_ids::QueueEntryId, repository_ids::RepositoryId,
    space_ids::SpaceId,
};
use shoal_protocol::{
    agent::Agent,
    approvals::ApprovalRequest,
    config::{
        modes::ModeName,
        permissions::{PermissionChange, PermissionsConfig},
    },
    model::{
        ByKind, Chat, Completion, MergeOptions, MergePlan, OpenedWorkspace, PortReservation,
        QueueEntry, QueueRequest, Relation, Repository, Space, UnmergePlan, Workspace,
        WorkspaceUsage,
    },
    progress::Progress,
    protocol::ScopeMemory,
    repository::Selector,
    space_names::{SpaceName, SpaceSelector},
    state::WorkspaceState,
};
use shoal_refs::CommitId;
use shoal_schedule::{
    priority::Priority,
    snapshot::Snapshot,
    stopping::{RunningDemo, RunningJob},
};
use shoal_store::lifecycle::Generation;
use tokio::sync::watch;

use crate::{
    Agents, capacity::Activity, daemon::*, execution_connection::RecordChange, model::QueuedJob,
};

pub(crate) use fixture::Fixture;
pub(crate) use records::{StandInRecords, StandInRemovals};

/// A daemon with the spaces and agents a test records.
#[derive(Default)]
pub(crate) struct StandIn {
    /// Each repository's layers over the global config, none for one a
    /// test gave none.
    repository_layers: Mutex<HashMap<RepositoryId, ConfigLayers>>,
    pub(crate) spaces: Mutex<Vec<Space>>,
    /// The registered repositories.
    pub(crate) repositories: Mutex<Vec<Repository>>,
    /// What the alerts told, by kind.
    pub(crate) notified: Mutex<Vec<(AlertKind, String)>>,
    /// What the alerts told the agents of spaces, by space and kind.
    pub(crate) told: Mutex<Vec<(SpaceId, AlertKind, String)>>,
    /// The failed state reports, each with the space it named.
    pub(crate) state_failures: Mutex<Vec<(SpaceSelector, String)>>,
    /// The service's store, which the workspaces' lifecycles live in.
    store: Mutex<Option<shoal_store::Store>>,
    /// The service, whose global config the settings resolve under and
    /// whose waiting entries the queue's rank reads.
    agents: Mutex<std::sync::Weak<Agents>>,
    /// Whether this machine hands its spaces over, as one joined to a hub.
    pub(crate) hands_over: std::sync::atomic::AtomicBool,
    /// The spaces each hand-over took.
    pub(crate) handings: Mutex<Vec<shoal_protocol::hub::packages::Handing>>,
    /// Why the machine left at each hand-over.
    pub(crate) leavings: Mutex<Vec<shoal_protocol::hub::packages::Leaving>>,
    /// What each stop of every CI job, to rerun, was for.
    pub(crate) job_stops: Mutex<Vec<String>>,
    /// Where the next hand-over says the spaces went.
    pub(crate) handed: Mutex<Vec<(Space, shoal_protocol::hub::packages::Handed)>>,
    /// Told as each hand-over starts sending.
    pub(crate) handing_started: tokio::sync::Notify,
    /// A hand-over that started sending ends only while a test holds no
    /// write lock on it.
    pub(crate) handing_gate: tokio::sync::RwLock<()>,
    /// How often the machine told the hub it stays.
    pub(crate) stays: std::sync::atomic::AtomicUsize,
    /// The running leases asleep agents hold, which memory pressure stops.
    pub(crate) asleep_leases: std::sync::atomic::AtomicUsize,
}

impl StandIn {
    fn find(&self, id: &SpaceId) -> Option<Space> {
        let spaces = self.spaces.lock().unwrap();
        spaces.iter().find(|space| space.id == *id).cloned()
    }

    /// Read and write the workspaces' lifecycles through the store of
    /// `agents`, as the daemon does through its own, and rank its queue.
    fn use_service(&self, agents: &Arc<Agents>) {
        *self.store.lock().unwrap() = Some(agents.store.clone());
        *self.agents.lock().unwrap() = Arc::downgrade(agents);
    }

    /// The service the fixture runs.
    fn service(&self) -> Arc<Agents> {
        let agents = self.agents.lock().unwrap().upgrade();
        agents.expect("the fixture gives the stand-in its service")
    }

    /// The settings of a space of `repository`, or of none.
    fn settings(&self, repository: Option<&RepositoryId>) -> Result<Effective> {
        let layers = self.repository_layers.lock().unwrap();
        let layers = repository.and_then(|id| layers.get(id)).cloned();
        (self.service().config.current()).resolve(&layers.unwrap_or_default())
    }

    /// The service's store.
    fn store(&self) -> shoal_store::Store {
        let store = self.store.lock().unwrap();
        store
            .clone()
            .expect("the fixture gives the stand-in its store")
    }

    /// Where the lifecycle of the workspace `id` stands, while it exists.
    async fn lifecycle(&self, id: &SpaceId) -> Result<Option<shoal_store::lifecycle::Lifecycle>> {
        let id = id.to_owned();
        let store = self.store();
        store
            .run(move |db| shoal_store::lifecycle::lifecycle(db, &id))
            .await
    }

    /// Record `state` as the workspace `id`'s, with `error`.
    async fn record_state(
        &self,
        id: &SpaceId,
        state: WorkspaceState,
        error: Option<String>,
    ) -> Result<()> {
        let id = id.to_owned();
        let store = self.store();
        store
            .write(move |tx| {
                tx.execute(
                    "UPDATE workspaces SET state=?2,error=?3 WHERE id=?1",
                    rusqlite::params![id, state, error],
                )?;
                Ok(())
            })
            .await
    }

    fn selected(&self, selector: &SpaceSelector) -> Result<Space> {
        let id = (selector.given_id()).with_context(|| format!("{NOT_HERE}: {selector}"))?;
        self.find(id).with_context(|| format!("{NOT_HERE}: {id}"))
    }
}

/// What a test reached that the stand-in does not offer.
const NOT_HERE: &str = "the agents' stand-in has no such space";

impl Spaces for StandIn {
    fn acknowledge_leftovers<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: &'a [u32],
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn stop_untracked_processes<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: Option<u32>,
    ) -> BoxFuture<'a, Result<usize>> {
        Box::pin(async { Ok(0) })
    }

    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        let space = self.selected(selector);
        Box::pin(async { space })
    }

    fn space_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>> {
        let space = self.find(id).with_context(|| format!("{NOT_HERE}: {id}"));
        Box::pin(async { space })
    }

    fn space_exists<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        let exists = self.find(id).is_some();
        Box::pin(async move { Ok(exists) })
    }

    fn agent_of<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<Agent>>> {
        Box::pin(async { Ok(None) })
    }

    fn space_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.settings(space.repository_id.as_ref());
        Box::pin(async { settings })
    }

    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        let workspace = self.selected(selector).and_then(|space| {
            let ByKind::Workspace(workspace) = space.by_kind() else {
                anyhow::bail!("{selector} is a chat");
            };
            Ok(workspace)
        });
        Box::pin(async { workspace })
    }

    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>> {
        let spaces = self.spaces.lock().unwrap().clone();
        Box::pin(async { Ok(spaces) })
    }

    fn visible_spaces<'a>(&'a self, _: Option<&'a Caller>) -> BoxFuture<'a, Result<Vec<Space>>> {
        let spaces = self.spaces.lock().unwrap().clone();
        Box::pin(async { Ok(spaces) })
    }

    fn workspace_below<'a>(
        &'a self,
        _: &'a Caller,
        _: &'a mut SpaceSelector,
        _: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// The whole queue, which the user reaches; the stand-in knows no
    /// scope.
    fn reachable_queue<'a>(
        &'a self,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<QueueEntry>>> {
        Box::pin(async move {
            anyhow::ensure!(caller.is_none(), "{NOT_HERE}: a scoped caller's queue");
            self.service().queue_entries(self).await
        })
    }

    fn workspace_settings<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.settings(Some(workspace.repository()));
        Box::pin(async { settings })
    }

    fn workspace_settings_with_templates<'a>(
        &'a self,
        workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.settings(Some(workspace.repository()));
        Box::pin(async { settings })
    }

    fn workspace_paths(&self) -> BoxFuture<'_, Result<Vec<PathBuf>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn chat_workspace<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: chat_workspace")) })
    }

    fn chat_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Chat>>> {
        let chat = self.find(id).and_then(|space| match space.by_kind() {
            ByKind::Chat(chat) => Some(chat),
            ByKind::Workspace(_) => None,
        });
        Box::pin(async { Ok(chat) })
    }

    fn chat_repository<'a>(&'a self, chat: &'a Chat) -> BoxFuture<'a, Result<Option<Repository>>> {
        Box::pin(async move {
            match &chat.repository_id {
                Some(id) => self.repository(id).await.map(Some),
                None => Ok(None),
            }
        })
    }

    /// The directory the chat's record says it runs in.
    fn chat_checkout_path<'a>(&'a self, chat: &'a Chat) -> BoxFuture<'a, Result<PathBuf>> {
        let path = chat.workdir.path().to_owned();
        Box::pin(async { Ok(path) })
    }

    fn chat_tmp_dir(&self, _: Option<&Repository>, _: &Chat) -> Result<Option<PathBuf>> {
        Ok(Default::default())
    }

    fn completion<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<Completion>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn reopen_for_resume<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn space_blocked<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn space_progress<'a>(
        &'a self,
        _: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<Progress>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn launch_priority<'a>(
        &'a self,
        _: &'a QueueRequest,
        _: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Option<Priority>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn ensure_change_held<'a>(
        &'a self,
        _: Option<&'a Caller>,
        _: &'a Space,
        _: &'a PermissionChange,
        _: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ensure_agent_held<'a>(
        &'a self,
        _: Option<&'a Caller>,
        _: &'a SpaceSelector,
        _: &'a ModeName,
        _: &'a PermissionsConfig,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn verify_space<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn prove_execution_ended<'a>(
        &'a self,
        _: &'a SpaceId,
        _: &'a ExecutionId,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(true) })
    }

    fn touch<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn run_agent_exit_hook<'a>(
        &'a self,
        _: &'a Space,
        _: &'a Agent,
        _: Option<i32>,
        _: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn workspace_hook<'a>(
        &'a self,
        _: &'a Workspace,
        _: HookKind,
    ) -> BoxFuture<'a, Result<Option<PathBuf>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn checkpoint_after_agent<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn workspace_usage(&self, _: &SpaceId) -> Option<WorkspaceUsage> {
        Default::default()
    }
}

impl Repositories for StandIn {
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>> {
        let repositories = self.repositories.lock().unwrap();
        let found = repositories.iter().find(|repository| repository.id == *id);
        let found = found.cloned().context(NOT_HERE);
        Box::pin(async { found })
    }

    /// The registered repository a selector names by its ID.
    fn selected_repository<'a>(
        &'a self,
        selector: &'a Selector,
    ) -> BoxFuture<'a, Result<Repository>> {
        let Selector::Id(id) = selector else {
            return Box::pin(async move { Err(anyhow::anyhow!("{NOT_HERE}: {selector:?}")) });
        };
        self.repository(id)
    }

    fn repository_settings<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.settings(Some(id));
        Box::pin(async { settings })
    }

    fn repository_layers<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<ConfigLayers>> {
        let layers = self.repository_layers.lock().unwrap().get(id).cloned();
        Box::pin(async { Ok(layers.unwrap_or_default()) })
    }
}

impl Alerts for StandIn {
    fn notify_space<'a>(
        &'a self,
        _: &'a SpaceName,
        _: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, ()> {
        self.notified.lock().unwrap().push((kind, message));
        Box::pin(async {})
    }

    fn report_state_failure<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        error: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        let mut failures = self.state_failures.lock().unwrap();
        failures.push((selector.clone(), error.to_owned()));
        Box::pin(async { Ok(()) })
    }

    fn notify<'a>(
        &'a self,
        _: Option<&'a SpaceName>,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, ()> {
        self.notified.lock().unwrap().push((kind, message));
        Box::pin(async {})
    }

    fn notify_about(&self, _: About, kind: AlertKind, message: String) -> BoxFuture<'_, ()> {
        self.notified.lock().unwrap().push((kind, message));
        Box::pin(async {})
    }

    /// Tell a space the stand-in records, whose agent hears it.
    fn tell_space<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: AlertKind,
        message: String,
    ) -> BoxFuture<'a, bool> {
        let heard = self.find(id).is_some();
        if heard {
            self.told.lock().unwrap().push((id.clone(), kind, message));
        }
        Box::pin(async move { heard })
    }

    fn notify_approval<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn ask_user_about<'a>(&'a self, _: &'a [ApprovalRequest]) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn tell_still_blocked<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Turns for StandIn {
    fn end_marked_turn<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: Option<&'a ExecutionId>,
        _: Option<String>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Lifecycles for StandIn {
    fn set_state<'a>(
        &'a self,
        id: &'a SpaceId,
        state: WorkspaceState,
        error: Option<String>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.record_state(id, state, error))
    }

    fn reserve_lifecycle<'a>(
        &'a self,
        id: &'a SpaceId,
        state: WorkspaceState,
    ) -> BoxFuture<'a, Result<WorkspaceState>> {
        let id = id.to_owned();
        let store = self.store();
        Box::pin(async move {
            store
                .write(move |tx| {
                    let lifecycle =
                        shoal_store::lifecycle::lifecycle(tx, &id)?.context(NOT_HERE)?;
                    let held = lifecycle.state;
                    anyhow::ensure!(
                        matches!(held, WorkspaceState::Ready | WorkspaceState::Failed),
                        "workspace is {held}"
                    );
                    tx.execute(
                        "UPDATE workspaces SET state=?2,error=NULL WHERE id=?1",
                        rusqlite::params![id, state],
                    )?;
                    Ok(held)
                })
                .await
        })
    }

    fn reserved_since<'a>(
        &'a self,
        id: &'a SpaceId,
        seen: Option<Generation>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            Ok(match self.lifecycle(id).await? {
                Some(lifecycle) => lifecycle.state.passes() || seen != Some(lifecycle.generation),
                None => seen.is_some(),
            })
        })
    }

    fn lifecycle_generation<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Generation>> {
        Box::pin(async move { Ok(self.lifecycle(id).await?.context(NOT_HERE)?.generation) })
    }

    fn check_lifecycle<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn await_reservation_end<'a>(
        &'a self,
        id: &'a SpaceId,
        wanted: &'a (dyn Fn() -> Result<()> + Send + Sync),
    ) -> BoxFuture<'a, Result<Generation>> {
        Box::pin(async move {
            let store = self.store();
            let mut changed = store.watch_changes();
            loop {
                wanted()?;
                match self.lifecycle(id).await?.context(NOT_HERE)? {
                    lifecycle if lifecycle.state.passes() => changed.changed().await?,
                    lifecycle => return Ok(lifecycle.generation),
                }
            }
        })
    }

    fn ensure_no_pending_rename<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn verify_worktree<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn reopen_workspace<'a>(
        &'a self,
        _: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<OpenedWorkspace>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: reopen_workspace")) })
    }

    fn refresh_admitted<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Operations for StandIn {
    fn start_waiting_preparations(self: Arc<Self>) -> BoxFuture<'static, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn tell_start_warnings<'a>(&'a self, _: &'a Space, _: &'a [String]) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Merges for StandIn {
    fn prepare_merge<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: &'a MergeOptions,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<MergePlan>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: prepare_merge")) })
    }

    fn prepare_unmerge<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, Result<UnmergePlan>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: prepare_unmerge")) })
    }

    fn authorize_scoped_merge<'a>(
        &'a self,
        _: &'a MergePlan,
        _: &'a Caller,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn authorize_scoped_unmerge<'a>(
        &'a self,
        _: &'a UnmergePlan,
        _: &'a Caller,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn update_merge_records<'a>(&'a self, _: &'a RecordChange) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn follow_pushed_merge_runs<'a>(&'a self, _: &'a RecordChange) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

impl Demos for StandIn {
    fn running_demos(&self) -> BoxFuture<'_, Result<Vec<RunningDemo>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn viewer_group<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, Result<Option<u32>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn retire_execution_viewer<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn reported_end<'a>(
        &'a self,
        _: &'a ExecutionId,
        _: Option<i32>,
    ) -> BoxFuture<'a, Option<i32>> {
        Box::pin(async { Default::default() })
    }

    fn unrun_demo_launch<'a>(
        &'a self,
        _: &'a ExecutionId,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn restart_demo_command<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn viewer_spawn_lock(&self, _: &ExecutionId) -> Arc<tokio::sync::Mutex<()>> {
        Default::default()
    }

    fn end_viewer(&self, _: &ExecutionId) {}
}

impl Links for StandIn {
    fn settle_agent(&self, _: &SpaceId) {}

    fn acknowledge_item_updates<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: Vec<String>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn agent_updates_recorded<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn updates_waiting<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn restart_watches<'a>(
        &'a self,
        _: &'a SpaceId,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn current_head<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<CommitId>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: current_head")) })
    }

    fn link_item<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: LinkKind,
        _: &'a ItemInput,
        _: Relation,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(async { Ok(Default::default()) })
    }
}

impl Runner for StandIn {
    fn job_executions(&self) -> BoxFuture<'_, Result<HashMap<SpaceId, ExecutionId>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn running_jobs(&self) -> BoxFuture<'_, Result<Vec<RunningJob>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn runs_job<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, bool> {
        Box::pin(async { Default::default() })
    }

    fn wake_runner_for_ended_job<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn job_workspaces(&self) -> BoxFuture<'_, HashSet<SpaceId>> {
        Box::pin(async { Default::default() })
    }

    fn stop_jobs_to_rerun(&self, why: JobStop) -> BoxFuture<'_, ()> {
        self.job_stops.lock().unwrap().push(why.to_string());
        Box::pin(async {})
    }

    fn cancel_job_in<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: JobStop,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn stop_job_for_overload<'a>(
        &'a self,
        _: &'a SpaceId,
        _: &'static str,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn wake_jobs(&self) {}

    fn job_permits_busy(&self, _: &RepositoryId) -> bool {
        Default::default()
    }

    fn job_claim_deferred(&self, _: &RepositoryId, _: &QueuedJob) -> Option<usize> {
        None
    }

    fn job_cost<'a>(
        &'a self,
        _: &'a RepositoryId,
        _: &'a QueuedJob,
    ) -> BoxFuture<'a, Result<JobCost>> {
        Box::pin(async { Err(anyhow::anyhow!("{NOT_HERE}: job_cost")) })
    }

    fn pass_over_job<'a>(
        &'a self,
        _: &'a QueueEntryId,
        _: &'a RepositoryId,
        _: &'a QueuedJob,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn job_activities(&self) -> BoxFuture<'_, Result<Vec<Activity>>> {
        Box::pin(async { Ok(Default::default()) })
    }
}

impl Leases for StandIn {
    fn release_execution_leases<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn release_ended_leases_of<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn stop_a_sleepers_running_lease(&self) -> BoxFuture<'_, bool> {
        use std::sync::atomic::Ordering;
        let taken = (self.asleep_leases).try_update(Ordering::SeqCst, Ordering::SeqCst, |held| {
            held.checked_sub(1)
        });
        Box::pin(async move { taken.is_ok() })
    }

    fn give_demo_its_leases<'a>(
        &'a self,
        _: &'a ExecutionId,
        _: &'a SpaceId,
        _: &'a shoal_protocol::demos::Launch,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn reserved_ports<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Vec<PortReservation>>> {
        Box::pin(async { Ok(Default::default()) })
    }
}

impl Reviews for StandIn {
    fn queue_unreviewed(&self, error: anyhow::Error) -> BoxFuture<'_, anyhow::Error> {
        Box::pin(async { error })
    }
}

impl Conversions for StandIn {
    fn stop_converting_agent<'a>(&'a self, _: &'a SpaceId, _: bool) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(Default::default()) })
    }

    fn ensure_takes_agents<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn hand_over_before_removal<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn moves_agent<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }
}

impl Memory for StandIn {
    fn record_scope_memory<'a>(&'a self, _: &'a ExecutionId, _: ScopeMemory) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn work_limit(&self) -> Option<WorkLimit> {
        Default::default()
    }
}

impl Ranking for StandIn {
    /// The queue's waiting entries, with no space ranked of its own and
    /// nothing waiting on anything, which the stand-in records none of.
    fn ranking_snapshot(&self) -> BoxFuture<'_, Result<Snapshot>> {
        let agents = self.service();
        Box::pin(async move {
            let oneshots = agents.agent_queue.waiting_oneshots();
            let entries = (agents.store)
                .read(move |tx| {
                    crate::queue::ranking::entries::waiting(tx, &HashMap::new(), &oneshots)
                })
                .await?;
            Ok(Snapshot {
                entries,
                now: shoal_protocol::time::UnixTime::now(),
                ..Snapshot::default()
            })
        })
    }
}

impl crate::daemon::Handovers for StandIn {
    fn hands_over(&self, _: shoal_protocol::hub::packages::Leaving) -> BoxFuture<'_, bool> {
        let hands_over = self.hands_over.load(std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move { hands_over })
    }

    fn hand_over(
        &self,
        _: tokio::time::Instant,
        handing: shoal_protocol::hub::packages::Handing,
        leaving: shoal_protocol::hub::packages::Leaving,
    ) -> BoxFuture<'_, Vec<(Space, shoal_protocol::hub::packages::Handed)>> {
        self.handings.lock().unwrap().push(handing);
        self.leavings.lock().unwrap().push(leaving);
        let handed = std::mem::take(&mut *self.handed.lock().unwrap());
        self.handing_started.notify_one();
        Box::pin(async move {
            let _gate = self.handing_gate.read().await;
            handed
        })
    }

    fn stay(&self) -> BoxFuture<'_, ()> {
        (self.stays).fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async {})
    }
}
