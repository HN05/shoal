//! A stand-in for the rest of the daemon, which the tests of leases build
//! the service over: one ready workspace, recorded in a temporary store,
//! whose repository's settings a test gives, and approval requests kept in
//! memory, which a test decides and which need a reason as the daemon's
//! do.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, ensure};
use futures_util::future::BoxFuture;
use rusqlite::Transaction;
use shoal_daemon_base::approval_requests::ReasonRequired;
use shoal_daemon_base::{
    config_state::ConfigState,
    notifications::{Coordinators, Notifications, Source},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::approvals::{ApprovalRequest, DecisionStatus, Target};
use shoal_protocol::notifications::NotificationKind;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

use crate::{
    Leases,
    config::{Config, Effective, repo::ConfigLayers},
    daemon::{Approvals, Grants, Settings, Spaces},
    model::Workspace,
};

/// The service over a stand-in daemon with one ready workspace.
pub(crate) struct Fixture {
    _root: tempfile::TempDir,
    pub(crate) leases: Leases,
    pub(crate) daemon: StandIn,
    pub(crate) grants: Arc<StandInGrants>,
    pub(crate) workspace: Workspace,
}

impl Fixture {
    /// A workspace whose repository's worktree file is `repo` under the
    /// global config `global`.
    pub(crate) async fn new(global: &str, repo: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = shoal_store::test_support::unsynced_store(root.path().join("state.db"))
            .await
            .unwrap();
        let workspace = workspace(root.path().join("worktree"));
        record(&store, &workspace).await;
        let config = Arc::new(ConfigState::new(shoal_core::test_support::global_config(
            global,
        )));
        let notifications = Arc::new(Notifications::new(store.clone()));
        let grants = Arc::new(StandInGrants::default());
        let leases = Leases::new(store, config, notifications, grants.clone());
        let daemon = StandIn {
            workspace: workspace.clone(),
            layers: Mutex::new(layers(repo)),
            touched: Mutex::default(),
            announced: Mutex::default(),
        };
        Self {
            _root: root,
            leases,
            daemon,
            grants,
            workspace,
        }
    }

    /// Make `repo` the worktree file of the workspace's repository.
    pub(crate) fn configure(&self, repo: &str) {
        *self.daemon.layers.lock().unwrap() = layers(repo);
    }

    pub(crate) fn selector(&self) -> SpaceSelector {
        SpaceSelector::id(&self.workspace.id)
    }

    /// The notifications recorded for the user, as kind and message.
    pub(crate) async fn notified(&self) -> Vec<(NotificationKind, String)> {
        let listed = self.leases.notifications.list(false, 100).await.unwrap();
        (listed.into_iter())
            .map(|notification| (notification.kind, notification.message))
            .collect()
    }
}

/// The layers of a repository whose worktree file is `repo`.
fn layers(repo: &str) -> ConfigLayers {
    ConfigLayers {
        worktree_file: crate::config::repo::parse(repo).unwrap(),
        ..ConfigLayers::default()
    }
}

/// A ready workspace with its worktree at `path`.
fn workspace(path: PathBuf) -> Workspace {
    serde_json::from_value(serde_json::json!({
        "id": "workspace",
        "number": 1,
        "name": "worker",
        "repository_id": "repo",
        "workdir": {
            "kind": "worktree",
            "workspace": {"id": "workspace", "name": "worker", "branch": "worker"},
            "path": path,
        },
        "kind": "workspace",
        "path": path,
        "branch": "worker",
        "state": "ready",
        "error": null,
        "base_commit": null,
        "base_ref": null,
        "git_dir": null,
    }))
    .unwrap()
}

/// Record `workspace` and its repository in `store`.
async fn record(store: &shoal_store::Store, workspace: &Workspace) {
    let path = workspace.path.to_str().unwrap().to_owned();
    store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO spaces(id,kind,repository_id,name)
                    VALUES ('workspace','workspace','repo','worker')",
                [],
            )?;
            tx.execute(
                "INSERT INTO workspaces(id,path,branch,state) VALUES ('workspace',?1,'worker','ready')",
                [path],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// The rest of the daemon as the fixture's workspace sees it.
pub(crate) struct StandIn {
    workspace: Workspace,
    layers: Mutex<ConfigLayers>,
    /// The workspaces whose requests counted as their activity.
    pub(crate) touched: Mutex<Vec<SpaceId>>,
    /// The approval requests announced to whoever decides them.
    pub(crate) announced: Mutex<Vec<ApprovalRequest>>,
}

impl Spaces for StandIn {
    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        Box::pin(async move {
            let selected = *selector == SpaceSelector::id(&self.workspace.id);
            selected
                .then(|| self.workspace.clone())
                .with_context(|| format!("no workspace {selector}"))
        })
    }

    fn touch<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, ()> {
        self.touched.lock().unwrap().push(id.clone());
        Box::pin(async {})
    }
}

impl Settings for StandIn {
    fn settings<'a>(
        &'a self,
        config: &'a Config,
        _workspace: &'a Workspace,
    ) -> BoxFuture<'a, Result<Effective>> {
        let layers = self.layers.lock().unwrap().clone();
        Box::pin(async move { config.resolve(&layers) })
    }
}

impl Approvals for StandIn {
    fn announce<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        self.announced.lock().unwrap().push(request.clone());
        Box::pin(async {})
    }
}

impl Coordinators for StandIn {
    fn tell<'a>(
        &'a self,
        _name: &'a SpaceName,
        _id: Option<&'a SpaceId>,
        _kind: NotificationKind,
        _message: String,
        _files: Vec<PathBuf>,
        _source: Source,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }
}

/// Approval requests by owner, target and name, which a test decides.
#[derive(Default)]
pub(crate) struct StandInGrants {
    requests: Mutex<HashMap<(SpaceId, Target, String), ApprovalRequest>>,
}

impl StandInGrants {
    /// Decide the request for `name` of `target`.
    pub(crate) fn decide(&self, target: &Target, name: &str, status: DecisionStatus) {
        let mut requests = self.requests.lock().unwrap();
        let key = (SpaceId::from("workspace"), target.clone(), name.to_owned());
        requests.get_mut(&key).expect("a request").status = status;
    }
}

impl Grants for StandInGrants {
    fn check(
        &self,
        _tx: &Transaction<'_>,
        request: ApprovalRequest,
    ) -> Result<Option<ApprovalRequest>> {
        let mut requests = self.requests.lock().unwrap();
        let key = (
            request.space_id.clone(),
            request.target.clone(),
            request.name.clone(),
        );
        if !requests.contains_key(&key) {
            ensure!(!request.reason.is_empty(), ReasonRequired);
        }
        let stored = requests.entry(key).or_insert(request);
        Ok((stored.status != DecisionStatus::Approved).then(|| stored.clone()))
    }

    fn release(
        &self,
        _tx: &Transaction<'_>,
        owner: &SpaceId,
        target: &Target,
        name: &str,
    ) -> Result<bool> {
        let key = (owner.clone(), target.clone(), name.to_owned());
        Ok(self.requests.lock().unwrap().remove(&key).is_some())
    }
}
