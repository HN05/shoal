//! A stand-in for the rest of the daemon, which the tests of the hub's own
//! operations build the service over: a daemon with no spaces, agents,
//! approvals or app store, with the repositories a test registers, acting
//! on the global config a test gives the service, and a temporary store
//! with no spaces in it.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_core::config::Config;
use shoal_daemon_base::{
    alerts::{NewAlert, Sent},
    callers::Caller,
    config_state::ConfigState,
    gates::Gates,
    notifications::{Coordinators, Notifications, Source},
};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::{
    alerts::AlertKind,
    approvals::ApprovalRequest,
    config::permissions::Permissions,
    files::SentFile,
    hub::{
        Member,
        reruns::{CutOffJob, CutOffRerun},
        routing::ChainedMode,
    },
    model::{QueueEntry, Repository, RunningAgent, Space},
    paths::Paths,
    protocol::{AppMethod, Request},
    saved::SavedSummary,
    space_names::{SpaceName, SpaceSelector},
    threads::Thread,
};
use shoal_schedule::{placement::snapshot::Capacity, priority::Priority};

use crate::{
    Hub,
    daemon::{
        Approvals, Apps, CutOffJobs, Dispatch, GlobalConfig, Messages, Packages, Queue, Receiver,
        Records, Registrations, SpacePermissions, Spaces,
    },
    streams::{Incoming, Outgoing},
};

/// How long a test waits for what should come at once before it fails, a
/// guard against hanging, not a limit it tests.
pub(crate) const HANG: Duration = Duration::from_secs(60);

/// The service over a store in `root` and the global `config`, whose
/// operations take [`StandIn`] for the daemon.
pub(crate) async fn hub(root: &tempfile::TempDir, config: Config) -> Arc<Hub> {
    let store = shoal_store::test_support::unsynced_store(root.path().join("state.sqlite3"))
        .await
        .unwrap();
    Arc::new(Hub::new(
        store.clone(),
        Arc::new(Gates::default()),
        Arc::new(ConfigState::new(config)),
        Arc::new(Notifications::new(store)),
        Paths::for_test(root.path()),
        Arc::new(StandInRecords),
    ))
}

/// A daemon with no spaces, agents, approvals or app store, and the
/// repositories a test registers.
#[derive(Default)]
pub(crate) struct StandIn {
    registered: std::sync::Mutex<Vec<Registered>>,
    /// The repositories whose local config a write fails to replace.
    refusing: std::sync::Mutex<HashSet<RepositoryId>>,
    /// The records of the files a test keeps here, with their paths.
    kept: Vec<SentFile>,
    /// What a rerun of a CI job another machine cut off finds, none for a
    /// machine without the user's login for its forge.
    rerun: std::sync::Mutex<Option<CutOffRerun>>,
}

/// A repository a test registers with the stand-in, with the text of its
/// local config.
#[derive(Clone)]
pub(crate) struct Registered {
    pub repository: Repository,
    pub local_config: Option<String>,
}

impl StandIn {
    /// A stand-in with `registered` repositories.
    pub(crate) fn registering(registered: Vec<Registered>) -> Self {
        Self {
            registered: std::sync::Mutex::new(registered),
            ..Self::default()
        }
    }

    /// A stand-in that keeps the files `kept` here.
    pub(crate) fn keeping(kept: Vec<SentFile>) -> Self {
        Self {
            kept,
            ..Self::default()
        }
    }

    /// Fail every write of the local config of the repository `id` while
    /// `refuse` holds.
    pub(crate) fn refuse_writes(&self, id: &RepositoryId, refuse: bool) {
        let mut refusing = self.refusing.lock().unwrap();
        match refuse {
            true => refusing.insert(id.clone()),
            false => refusing.remove(id),
        };
    }

    /// Have each rerun of a CI job another machine cut off find `rerun`.
    pub(crate) fn rerun_finds(&self, rerun: CutOffRerun) {
        *self.rerun.lock().unwrap() = Some(rerun);
    }

    /// The repositories registered now.
    pub(crate) fn registered(&self) -> Vec<Registered> {
        self.registered.lock().unwrap().clone()
    }

    /// Change the registration `id` as `change` does.
    fn change(&self, id: &RepositoryId, change: impl FnOnce(&mut Registered)) -> Result<()> {
        let mut registered = self.registered.lock().unwrap();
        let found = registered.iter_mut().find(|r| &r.repository.id == id);
        let Some(found) = found else {
            bail!("the stand-in daemon has no repository {id}");
        };
        change(found);
        Ok(())
    }

    /// The text of the local config of the registration `id`.
    fn local_config(&self, id: &RepositoryId) -> Result<Option<String>> {
        let registered = self.registered.lock().unwrap();
        let found = registered.iter().find(|r| &r.repository.id == id);
        match found {
            Some(found) => Ok(found.local_config.clone()),
            None => bail!("the stand-in daemon has no repository {id}"),
        }
    }
}

/// What a stand-in that has none of it answers when asked for `what`.
fn none<T>(what: &str) -> BoxFuture<'static, Result<T>> {
    let message = format!("the stand-in daemon has no {what}");
    Box::pin(async move { bail!(message) })
}

impl Registrations for StandIn {
    fn register_clone(&self, _: String, _: String) -> BoxFuture<'_, Result<Repository>> {
        none("repositories to clone")
    }

    fn share_repository_as<'a>(
        &'a self,
        id: &'a RepositoryId,
        url: Option<String>,
    ) -> BoxFuture<'a, Result<()>> {
        let shared = self.change(id, |found| found.repository.shared_url = url);
        Box::pin(async move { shared })
    }

    fn local_config_text<'a>(
        &'a self,
        id: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        let text = self.local_config(id);
        Box::pin(async move { text })
    }

    /// Keeps a replaced file at `<repository directory>/config.toml.backup`.
    fn take_local_config_copy<'a>(
        &'a self,
        id: &'a RepositoryId,
        expected: Option<String>,
        text: Option<String>,
        keep: bool,
    ) -> BoxFuture<'a, Result<Option<std::path::PathBuf>>> {
        let taken = (|| {
            if self.local_config(id)? != expected {
                bail!("the local config of {id} changed");
            }
            if self.refusing.lock().unwrap().contains(id) {
                bail!("the local config of {id} cannot be written");
            }
            let mut kept = None;
            self.change(id, |found| {
                let backup = found.repository.workspaces_dir.join("config.toml.backup");
                kept = (keep && found.local_config.is_some()).then_some(backup);
                found.local_config = text;
            })?;
            Ok(kept)
        })();
        Box::pin(async move { taken })
    }
}

impl Spaces for StandIn {
    fn space<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Space>> {
        none("spaces")
    }

    fn select_space<'a>(&'a self, _: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        none("spaces")
    }

    fn watch_agents(&self) -> tokio::sync::watch::Receiver<u64> {
        tokio::sync::watch::channel(0).1
    }

    fn find_space<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>> {
        Box::pin(async { Ok(None) })
    }

    fn listed_spaces<'a>(&'a self, _: Option<&'a Caller>) -> BoxFuture<'a, Result<Vec<Space>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn spaces_within<'a>(&'a self, _: Option<&'a SpaceId>) -> BoxFuture<'a, Result<Vec<Space>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn priority<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Priority>> {
        none("spaces")
    }

    fn reach_root<'a>(&'a self, _: Option<&'a Caller>) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(async { Ok(None) })
    }

    fn coordinator<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(async { Ok(None) })
    }

    fn receiver_from<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<Receiver>>> {
        Box::pin(async { Ok(None) })
    }

    fn running_agents(&self) -> BoxFuture<'_, Result<Vec<RunningAgent>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn with_agents(&self) -> BoxFuture<'_, Result<HashSet<SpaceId>>> {
        Box::pin(async { Ok(HashSet::new()) })
    }

    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        let registered = self.registered().into_iter();
        let repositories = registered.map(|r| r.repository).collect();
        Box::pin(async { Ok(repositories) })
    }

    fn acts_on_global_config(&self) -> bool {
        true
    }
}

impl SpacePermissions for StandIn {
    fn caller_permissions<'a>(
        &'a self,
        _: &'a Caller,
        _: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Permissions>> {
        none("spaces")
    }

    fn chained_permissions<'a>(
        &'a self,
        _: &'a [ChainedMode],
        _: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Permissions>> {
        none("repositories")
    }

    fn chat_modes<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<Vec<ChainedMode>>> {
        none("spaces")
    }

    fn space_permissions<'a>(
        &'a self,
        _: &'a SpaceId,
        _: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<Permissions>> {
        none("spaces")
    }
}

impl Messages for StandIn {
    fn message<'a>(&'a self, _: &'a SpaceId, _: AlertKind, _: String) -> BoxFuture<'a, Result<()>> {
        none("agents")
    }

    fn message_next_turn<'a>(
        &'a self,
        _: &'a SpaceId,
        _: AlertKind,
        _: String,
    ) -> BoxFuture<'a, Result<()>> {
        none("agents")
    }

    fn alert<'a>(&'a self, _: &'a SpaceId, _: NewAlert, _: bool) -> BoxFuture<'a, Result<()>> {
        none("agents")
    }

    fn kept_copies(&self, files: Vec<SentFile>) -> BoxFuture<'_, Result<Vec<SentFile>>> {
        let kept = (files.iter())
            .filter_map(|file| {
                (self.kept.iter()).find(|kept| (kept.id, &kept.space) == (file.id, &file.space))
            })
            .cloned()
            .collect();
        Box::pin(async { Ok(kept) })
    }
}

impl Approvals for StandIn {
    fn approval_requests<'a>(
        &'a self,
        _: Option<&'a SpaceSelector>,
    ) -> BoxFuture<'a, Result<Vec<ApprovalRequest>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn open_threads(&self) -> BoxFuture<'_, Result<Vec<Thread>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn pass_over_offline<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>> {
        none("approval requests")
    }

    fn pass_to_user<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>> {
        none("approval requests")
    }

    fn saved_summaries<'a>(
        &'a self,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<SavedSummary>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn visible_queue(&self) -> BoxFuture<'_, Result<Vec<QueueEntry>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

impl Dispatch for StandIn {
    fn answer(&self, _: Request) -> BoxFuture<'_, Result<Vec<serde_json::Value>>> {
        none("requests to answer")
    }

    fn stream<'a>(
        &'a self,
        _: Request,
        _: &'a mut Outgoing,
        _: Option<Incoming>,
        _: Box<dyn FnOnce() + Send + 'a>,
    ) -> BoxFuture<'a, Result<()>> {
        none("requests to answer")
    }
}

impl Apps for StandIn {
    fn serve_app_request(
        self: Arc<Self>,
        _: &Member,
        _: AppMethod,
        _: Option<String>,
        _: Outgoing,
    ) -> Result<()> {
        bail!("the stand-in daemon has no app store")
    }
}

impl Packages for StandIn {
    fn package_state<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { bail!("the stand-in daemon has no spaces to package") })
    }

    fn write_package<'a>(
        &'a self,
        _: &'a Space,
        _: &'a std::path::Path,
    ) -> BoxFuture<'a, Result<shoal_protocol::model::RemovedSpace>> {
        Box::pin(async { bail!("the stand-in daemon has no spaces to package") })
    }

    fn retire_space<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { bail!("the stand-in daemon has no spaces to retire") })
    }

    fn package_start<'a>(
        &'a self,
        _: &'a Space,
        _: &'a shoal_protocol::model::RemovedSpace,
    ) -> BoxFuture<'a, Result<shoal_schedule::placement::snapshot::Request>> {
        Box::pin(async { bail!("the stand-in daemon hands no spaces over") })
    }

    fn continue_handed_over<'a>(
        &'a self,
        _: &'a SpaceId,
        _: &'a str,
        _: shoal_protocol::hub::packages::Departure,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { bail!("the stand-in daemon continues no spaces") })
    }
}

impl Queue for StandIn {
    fn queue_room(&self) -> BoxFuture<'_, Result<Capacity>> {
        Box::pin(async { Ok(Capacity::default()) })
    }
}

impl GlobalConfig for StandIn {
    fn reload_config(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl CutOffJobs for StandIn {
    fn rerun_cut_off<'a>(&'a self, _: &'a CutOffJob) -> BoxFuture<'a, Result<CutOffRerun>> {
        let rerun = self.rerun.lock().unwrap().unwrap_or(CutOffRerun::NoLogin);
        Box::pin(async move { Ok(rerun) })
    }
}

impl Coordinators for StandIn {
    fn tell<'a>(&'a self, _: NewAlert, _: bool, _: Source) -> BoxFuture<'a, Option<Sent>> {
        Box::pin(async { None })
    }
}

/// The records of a store with no spaces in it.
struct StandInRecords;

impl Records for StandInRecords {
    fn parent_id(&self, _: &Connection, _: &SpaceId) -> Result<Option<SpaceId>> {
        Ok(None)
    }

    fn parent_ids(&self, _: &Connection) -> Result<HashMap<SpaceId, SpaceId>> {
        Ok(HashMap::new())
    }

    fn below_elsewhere(&self, _: &Connection) -> Result<Vec<(SpaceId, SpaceId)>> {
        Ok(Vec::new())
    }

    fn first_absent_above(&self, _: &Connection, _: &SpaceId) -> Result<Option<SpaceId>> {
        Ok(None)
    }

    fn reserved_numbers(&self, _: &Connection) -> Result<Vec<SpaceNumber>> {
        Ok(Vec::new())
    }

    fn kept_files(&self, _: &Connection) -> Result<Vec<SpaceId>> {
        Ok(Vec::new())
    }

    fn space_number(
        &self,
        _: &Connection,
        _: &SpaceId,
    ) -> Result<Option<(SpaceName, SpaceNumber)>> {
        Ok(None)
    }

    fn renumber_space(
        &self,
        _: &Transaction<'_>,
        _: &SpaceId,
        _: SpaceNumber,
        _: SpaceNumber,
    ) -> Result<()> {
        bail!("the stand-in store has no spaces")
    }
}
