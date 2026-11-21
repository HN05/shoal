//! A stand-in for the rest of the daemon, which the tests of the hub's own
//! operations build the service over: a daemon with no spaces, agents,
//! approvals or app store, acting on the global config a test gives the
//! service, and a temporary store with no spaces in it.
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
    hub::{Member, routing::ChainedMode},
    model::{QueueEntry, Repository, RunningAgent, Space},
    paths::Paths,
    protocol::{AppMethod, Request},
    saved::SavedSummary,
    space_names::{SpaceName, SpaceSelector},
    threads::Thread,
};
use shoal_schedule::priority::Priority;

use crate::{
    Hub,
    daemon::{
        Approvals, Apps, Dispatch, Messages, Packages, Receiver, Records, SpacePermissions, Spaces,
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

/// A daemon with no spaces, agents, approvals or app store.
pub(crate) struct StandIn;

/// What a stand-in that has none of it answers when asked for `what`.
fn none<T>(what: &str) -> BoxFuture<'static, Result<T>> {
    let message = format!("the stand-in daemon has no {what}");
    Box::pin(async move { bail!(message) })
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
        Box::pin(async { Ok(Vec::new()) })
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
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { bail!("the stand-in daemon has no spaces to package") })
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
