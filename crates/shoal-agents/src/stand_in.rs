//! A stand-in for the rest of the daemon, which the agents' tests build the
//! service over: a home with its state directory under a temporary
//! directory, the global config a test gives, the workspaces a test
//! records, the agents a test registers with their executions, and alerts
//! that only note what they tell.
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_core::{
    config::{Config, Effective, repo::ConfigLayers},
    paths::StateFiles,
    process::identity::AgentTrees,
};
use shoal_daemon_base::{alerts::AlertKind, gates::Gates, signals::Signals};
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::{
    agent::Agent,
    model::{Space, SpacePart, Workspace},
    paths::Paths,
    space_names::{SpaceName, SpaceSelector},
};

use crate::{
    Agents,
    daemon::{Alerts, Executions, Queue, Records, Spaces, Turns},
};

/// The service over a stand-in daemon.
pub(crate) struct Fixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) agents: Agents,
    pub(crate) daemon: StandIn,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        Self::with_config("").await
    }

    /// The service under the global config `text` adds to the tests' base.
    pub(crate) async fn with_config(text: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(std::fs::canonicalize(root.path()).unwrap());
        std::fs::create_dir_all(&paths.state).unwrap();
        let daemon = StandIn {
            config: shoal_core::test_support::global_config(text),
            ..StandIn::default()
        };
        Self {
            root,
            agents: service(paths).await,
            daemon,
        }
    }

    /// The service a daemon that restarts builds over the same state,
    /// knowing only what it recorded there.
    pub(crate) async fn restart(self) -> Self {
        let paths = self.agents.paths.clone();
        drop(self.agents);
        Self {
            agents: service(paths).await,
            ..self
        }
    }

    /// Record a ready workspace `name`, of a repository recorded with it.
    pub(crate) async fn workspace(&self, name: &str) -> Workspace {
        let path = self.root.path().join(name);
        let workspace = shoal_protocol::test_support::workspace(name, "repo", name, &path);
        let id = workspace.id.clone();
        (self.agents.store)
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
        let mut spaces = self.daemon.spaces.lock().unwrap();
        spaces.push(Space::from(workspace.clone()));
        workspace
    }

    /// Change the space `id` as `change` does, as the daemon reads it.
    pub(crate) fn update_space(&self, id: &SpaceId, change: impl FnOnce(&mut Space)) {
        let mut spaces = self.daemon.spaces.lock().unwrap();
        change(spaces.iter_mut().find(|space| space.id == *id).unwrap());
    }

    /// Record that the work of the space `id` is done, as `shoal done` does.
    pub(crate) async fn done(&self, id: &SpaceId) {
        let recorded = id.clone();
        (self.agents.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO space_completion(space_id,record) VALUES (?1,'{}')",
                    [&recorded],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        self.update_space(id, |space| {
            if let SpacePart::Workspace(workspace) = &mut space.part {
                workspace.done = true;
            }
        });
    }

    /// Record a chat `name`, without a repository, whose agent runs in an
    /// execution of the same name.
    pub(crate) async fn chat(&self, name: &str) -> SpaceId {
        let id = SpaceId::from(name);
        let recorded = id.clone();
        (self.agents.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO spaces(id,kind,name) VALUES (?1,'chat',?1)",
                    [&recorded],
                )?;
                tx.execute(
                    "INSERT INTO executions(id,space_id,state) VALUES (?1,?1,'running')",
                    [&recorded],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        id
    }

    /// Record a running execution `id` in `workspace` whose wrapper
    /// registered `agent`, when it gives one.
    pub(crate) async fn execution(
        &self,
        workspace: &Workspace,
        id: &str,
        agent: Option<Agent>,
    ) -> ExecutionId {
        let (space, execution) = (workspace.id.clone(), ExecutionId::from(id));
        let recorded = execution.clone();
        (self.agents.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO executions(id,space_id,state) VALUES (?1,?2,'running')",
                    rusqlite::params![recorded, space],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        if let Some(agent) = agent {
            let mut agents = self.daemon.agents.lock().unwrap();
            agents.insert(execution.clone(), agent);
        }
        execution
    }
}

/// The service over the state directory of `paths`.
async fn service(paths: Paths) -> Agents {
    let store = shoal_store::test_support::unsynced_store(paths.database())
        .await
        .unwrap();
    Agents::new(
        store,
        Arc::new(Gates::default()),
        Arc::new(Signals::default()),
        paths,
        Arc::new(StandInRecords),
    )
}

/// A daemon with the spaces and agents a test records.
#[derive(Default)]
pub(crate) struct StandIn {
    /// The config every space's settings resolve under.
    pub(crate) config: Config,
    pub(crate) spaces: Mutex<Vec<Space>>,
    /// The agent each execution registered.
    pub(crate) agents: Mutex<HashMap<ExecutionId, Agent>>,
    /// The executions whose agents Shoal is stopping.
    pub(crate) stopping: Mutex<HashSet<ExecutionId>>,
    /// What the alerts told, by kind.
    pub(crate) notified: Mutex<Vec<(AlertKind, String)>>,
    /// The failed state reports, each with the space it named.
    pub(crate) state_failures: Mutex<Vec<(SpaceSelector, String)>>,
}

impl StandIn {
    fn find(&self, id: &SpaceId) -> Option<Space> {
        let spaces = self.spaces.lock().unwrap();
        spaces.iter().find(|space| space.id == *id).cloned()
    }

    fn selected(&self, selector: &SpaceSelector) -> Result<Space> {
        let id = selector.given_id().context(NOT_HERE)?;
        self.find(id).context(NOT_HERE)
    }
}

/// What a test reached that the stand-in does not offer.
const NOT_HERE: &str = "the agents' stand-in has no such space";

impl Spaces for StandIn {
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        let space = self.selected(selector);
        Box::pin(async { space })
    }

    fn space_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Space>> {
        let space = self.find(id).context(NOT_HERE);
        Box::pin(async { space })
    }

    fn space_exists<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        let exists = self.find(id).is_some();
        Box::pin(async move { Ok(exists) })
    }

    fn agent_of<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Option<Agent>>> {
        Box::pin(async { Ok(None) })
    }

    fn space_settings<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.config.resolve(&ConfigLayers::default());
        Box::pin(async { settings })
    }
}

impl Executions for StandIn {
    fn agent_name<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, Option<Agent>> {
        let agent = self.agents.lock().unwrap().get(id).cloned();
        Box::pin(async { agent })
    }

    fn agent_stopping<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, bool> {
        let stopping = self.stopping.lock().unwrap().contains(id);
        Box::pin(async move { stopping })
    }

    fn agent_trees(&self) -> BoxFuture<'_, Result<AgentTrees>> {
        Box::pin(async { Ok(AgentTrees::default()) })
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

impl Queue for StandIn {
    fn wake_queue(&self) {}
}

/// Records without the other subsystems' tables: no commands an agent
/// left, and no restores Shoal queued.
struct StandInRecords;

impl Records for StandInRecords {
    fn end_commands(&self, _: &Transaction<'_>, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn stop_recorded(&self, db: &Connection, id: &ExecutionId) -> Result<bool> {
        Ok(db.query_row(
            "SELECT EXISTS (SELECT 1 FROM agent_stops WHERE execution_id=?1)",
            [id],
            |row| row.get(0),
        )?)
    }

    fn wake_asleep(&self, _: &Connection, _: &SpaceId) -> Result<()> {
        Ok(())
    }
}
