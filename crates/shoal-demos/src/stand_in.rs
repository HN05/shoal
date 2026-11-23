//! A stand-in for the rest of the daemon, which the tests of demos build
//! the service over: a home with its state directory under a temporary
//! directory, the workspaces a test records, each demo's execution recorded
//! with its scope token and run from the test process, and the ports a test
//! reserves, kept in memory. Stopping and inspecting executions is the
//! daemon's, which tests across subsystems reach.
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use futures_util::future::BoxFuture;
use shoal_daemon_base::callers::Caller;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::demos::{Launch, Running};
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    Demos,
    daemon::{Executions, Joining, Ports, Proof, Reserved, Spaces},
    model::{Execution, PortRequest, Space, Workspace},
    paths::{Paths, StateFiles as _},
    process::{execution::Processes, identity::Scan},
    records,
};

/// The service over a stand-in daemon.
pub(crate) struct Fixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) demos: Arc<Demos>,
    pub(crate) daemon: Arc<StandIn>,
    pub(crate) paths: Paths,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(std::fs::canonicalize(root.path()).unwrap());
        std::fs::create_dir_all(&paths.state).unwrap();
        let store = shoal_store::test_support::unsynced_store(paths.database())
            .await
            .unwrap();
        Self {
            root,
            demos: Arc::new(Demos::new(store, paths.clone())),
            daemon: Arc::default(),
            paths,
        }
    }

    /// Record a ready workspace `name` with its directory under the root.
    pub(crate) async fn workspace(&self, name: &str) -> Workspace {
        let path = self.root.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let workspace = shoal_protocol::test_support::workspace(name, "repo", name, &path);
        let id = workspace.id.clone();
        (self.demos.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO repositories(id,path,source,last_used)
                     VALUES ('repo','/repo','/repo',1)",
                    [],
                )?;
                tx.execute(
                    "INSERT INTO spaces(id,kind,repository_id,name)
                     VALUES (?1,'workspace','repo',?1)",
                    [&id],
                )?;
                tx.execute(
                    "INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
                     VALUES (?1,'/work/'||?1,?1,'ready','abc','/admin')",
                    [&id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let space = shoal_protocol::test_support::space(&workspace);
        self.daemon.spaces.lock().unwrap().push(space);
        workspace
    }

    /// Record `launch` as a demo of `workspace`, run by an execution of its
    /// own, as listings show it.
    pub(crate) async fn demo(&self, workspace: &SpaceId, launch: Launch) -> Running {
        let execution = ExecutionId::generate();
        let (space, recorded) = (workspace.clone(), execution.clone());
        (self.demos.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO executions(id,space_id,state,scope_token)
                     VALUES (?1,?2,'running',?3)",
                    rusqlite::params![recorded, space, ScopeToken::generate()],
                )?;
                records::record(tx, &recorded, &space, &launch)
            })
            .await
            .unwrap();
        let listed = self.demos.list(&*self.daemon, None).await.unwrap();
        (listed.into_iter())
            .find(|demo| demo.execution_id == execution)
            .expect("the demo runs")
    }
}

/// The rest of the daemon, as the fixture records it.
#[derive(Default)]
pub(crate) struct StandIn {
    spaces: Mutex<Vec<Space>>,
    /// The executions a test says are completing.
    completing: Mutex<HashSet<ExecutionId>>,
    /// The ports reserved, by name.
    ports: Mutex<BTreeMap<String, u16>>,
}

impl StandIn {
    /// Say that `execution` is completing, as its wrapper's end does.
    pub(crate) fn complete(&self, execution: &ExecutionId) {
        self.completing.lock().unwrap().insert(execution.clone());
    }

    fn find(&self, id: &SpaceId) -> Option<Space> {
        let spaces = self.spaces.lock().unwrap();
        spaces.iter().find(|space| space.id == *id).cloned()
    }

    fn workspace(&self, id: &SpaceId) -> Result<Workspace> {
        let space = self.find(id).with_context(|| format!("{NOT_HERE}: {id}"))?;
        (space.into_workspace()).map_err(|_| anyhow::anyhow!("{NOT_HERE}: chat {id}"))
    }
}

/// What a test reached that the stand-in does not offer.
const NOT_HERE: &str = "the demos' stand-in has no such space";

impl Spaces for StandIn {
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        let space = (selector.given_id())
            .and_then(|id| self.find(id))
            .with_context(|| format!("{NOT_HERE}: {selector}"));
        Box::pin(async { space })
    }
}

impl Executions for StandIn {
    fn launch_ended_unrun(&self, _: &str) -> bool {
        false
    }

    fn completing(&self, execution: &ExecutionId) -> bool {
        self.completing.lock().unwrap().contains(execution)
    }

    fn ask_to_stop(
        &self,
        executions: Vec<ExecutionId>,
        _: String,
    ) -> BoxFuture<'_, Vec<ExecutionId>> {
        Box::pin(async { executions })
    }

    fn stop_disconnected<'a>(&'a self, _: &'a Execution, _: Proof) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { bail!("{NOT_HERE}: stopping executions") })
    }

    fn inspect_execution<'a>(
        &'a self,
        _: &'a Execution,
        _: &'a Scan,
    ) -> BoxFuture<'a, Result<Processes>> {
        Box::pin(async { bail!("{NOT_HERE}: inspecting executions") })
    }

    fn forget_execution<'a>(&'a self, _: &'a ExecutionId) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { bail!("{NOT_HERE}: forgetting executions") })
    }

    fn joining<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Joining>> {
        let joining = self.workspace(id).map(|workspace| Joining {
            space: (&workspace).into(),
            configured: BTreeMap::new(),
            ports: Vec::new(),
        });
        Box::pin(async { joining })
    }
}

impl Ports for StandIn {
    fn reserve_port<'a>(
        &'a self,
        _: &'a SpaceId,
        name: String,
        request: PortRequest,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Reserved>> {
        let mut ports = self.ports.lock().unwrap();
        let port = *ports.entry(name).or_insert_with(|| {
            request.port.unwrap_or_else(|| {
                // One the system gave out, so tests running at once do not
                // reach each other's viewers.
                let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                free.local_addr().unwrap().port()
            })
        });
        Box::pin(async move { Ok(Reserved::Port(port)) })
    }
}
