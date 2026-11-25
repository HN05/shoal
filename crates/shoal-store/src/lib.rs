//! SQLite persistence: schema migrations, row mappers, and the small
//! guards every mutation shares.
use shoal_core::{config, process, refusal};
use shoal_protocol::{agent, model, state};

use crate::refusal::{Reason, Refusal};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Params, Row, Transaction, TransactionBehavior};
use serde::{Serialize, de::DeserializeOwned};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_refs::CommitId;
use std::{collections::HashMap, path::PathBuf, time::Duration};

use crate::{
    model::{Base, Checkout, Execution, Head, PortReservation, Repository, Workspace},
    state::{ExecutionState, WorkspaceState},
};
use shoal_protocol::space_names::SpaceName;

pub mod agent_handoffs;
#[cfg(test)]
mod benchmark;
#[cfg(test)]
mod event_tests;
pub mod lifecycle;
mod migrations;
pub use migrations::SCHEMA_VERSION;
pub mod sent_files;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod tree;
mod worker;

/// Whether the store's commits sync to disk, which tests skip.
#[derive(Clone, Copy)]
enum Syncs {
    Kept,
    Skipped,
}

#[derive(Clone)]
pub struct Store {
    worker: std::sync::Arc<worker::Worker>,
}

impl Store {
    /// Open persistence and migrate its schema without changing active operations.
    pub async fn open(path: PathBuf) -> Result<Self> {
        let syncs = match cfg!(test) {
            true => Syncs::Skipped,
            false => Syncs::Kept,
        };
        Self::open_with(path, syncs).await
    }

    async fn open_with(path: PathBuf, syncs: Syncs) -> Result<Self> {
        let store = Self {
            worker: worker::Worker::start(path, syncs)?,
        };
        // Not `write`: tests migrate plain connections, so the migration
        // opens its own transaction.
        store
            .run(migrations::migrate)
            .await
            .context("migrate Shoal state database")?;
        Ok(store)
    }

    /// Run once on the connection's blocking worker, waiting for queue space.
    /// Prefer `write` or `read`; this suits an operation that manages its own
    /// transactions or needs none.
    pub async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.worker.run(operation).await
    }

    /// Run once in an immediate transaction that commits when the operation
    /// succeeds and rolls back when it fails.
    pub async fn write<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.run(|db| write_transaction(db, operation)).await
    }

    /// Run once in a deferred transaction, so every query reads one snapshot.
    pub async fn read<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.run(|db| {
            let tx = db.transaction()?;
            let value = operation(&tx)?;
            tx.commit()?;
            Ok(value)
        })
        .await
    }

    pub fn watch_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.worker.changed.subscribe()
    }

    /// How many waits watch the store's changes now, which tells a test
    /// that a wait it started waits.
    #[cfg(any(test, feature = "test-support"))]
    pub fn change_watchers(&self) -> usize {
        self.worker.changed.receiver_count()
    }

    pub async fn shutdown(&self) {
        self.worker.shutdown().await;
    }
}

/// Run `operation` in an immediate transaction on `db`, as [`Store::write`]
/// does, for an operation that runs several transactions on the worker.
pub fn write_transaction<T>(
    db: &mut Connection,
    operation: impl FnOnce(&Transaction<'_>) -> Result<T>,
) -> Result<T> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let value = operation(&tx)?;
    tx.commit()?;
    Ok(value)
}

/// Whether the supplied SELECT query returns any rows.
pub fn exists(db: &Connection, sql: &str, params: impl Params) -> Result<bool> {
    Ok(db
        .prepare_cached(&format!("SELECT EXISTS({sql})"))?
        .query_row(params, |row| row.get(0))?)
}

/// Refuse a new workspace path that contains or lies inside a recorded
/// workspace's path, whether or not that directory still exists.
pub fn ensure_path_unrecorded(db: &Connection, path: &std::path::Path) -> Result<()> {
    let recorded = db
        .prepare("SELECT path FROM workspaces")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for other in recorded.iter().map(std::path::Path::new) {
        ensure!(
            !path.starts_with(other) && !other.starts_with(path),
            "workspace path overlaps a recorded workspace: {}",
            other.display()
        );
    }
    Ok(())
}

/// Fail while a workspace has an unfinished rename; lifecycle operations
/// must wait for repair.
pub fn ensure_no_pending_rename(db: &Connection, workspace_id: &SpaceId) -> Result<()> {
    ensure!(
        !exists(
            db,
            "SELECT 1 FROM workspace_renames WHERE workspace_id=?1",
            [workspace_id]
        )?,
        "workspace has an unfinished rename; run shoal branch rename again to finish it"
    );
    Ok(())
}

/// Refuse lifecycle changes while the workspace's repository directory moves.
pub fn ensure_no_pending_move(db: &Connection, workspace_id: &SpaceId) -> Result<()> {
    ensure!(
        !exists(
            db,
            "SELECT 1 FROM repository_moves JOIN spaces USING (repository_id)
                WHERE spaces.id=?1 AND spaces.kind='workspace'",
            [workspace_id]
        )?,
        "repository move is incomplete; run shoal repo move again"
    );
    Ok(())
}

/// Refuse new users of a repository whose directory is moving.
pub fn ensure_repository_not_moving(db: &Connection, repository_id: &RepositoryId) -> Result<()> {
    ensure!(
        !exists(
            db,
            "SELECT 1 FROM repository_moves WHERE repository_id=?1",
            [repository_id]
        )?,
        "repository move is incomplete; run shoal repo move again"
    );
    Ok(())
}

/// Refuse a change to the records of the space `id`, such as its links or
/// threads, while it is a workspace that is not ready; a chat takes one
/// whenever it is recorded.
pub fn require_ready_space(db: &Connection, id: &SpaceId) -> Result<()> {
    if exists(db, "SELECT 1 FROM spaces WHERE id=?1 AND kind='chat'", [id])? {
        return Ok(());
    }
    require_ready(db, id)
}

/// Fail unless the workspace is ready; resources may only change then.
pub fn require_ready(db: &Connection, workspace_id: &SpaceId) -> Result<()> {
    let (number, name, state): (shoal_ids::space_ids::SpaceNumber, SpaceName, WorkspaceState) = db
        .query_row(
            "SELECT s.number, s.name, w.state FROM workspaces w JOIN spaces s ON s.id=w.id
             WHERE w.id=?1",
            [workspace_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    match state {
        WorkspaceState::Ready => Ok(()),
        state => Err(not_ready(number, workspace_id, &name, state)),
    }
}

/// The refusal of the workspace numbered `number`, `id`, named `name`, in
/// `state`, by a request that needs it ready, naming what to do about it.
/// The daemon's check of a request's workspace and each recheck in a
/// transaction refuse with it alike.
pub fn not_ready(
    number: shoal_ids::space_ids::SpaceNumber,
    id: &SpaceId,
    name: &SpaceName,
    state: WorkspaceState,
) -> anyhow::Error {
    let remedy = match state {
        WorkspaceState::Failed => {
            format!("shoal status {number} shows why, and shoal setup {number} retries its setup")
        }
        WorkspaceState::Preparing => format!(
            "run the command again once it is ready; shoal status {number} shows how far it got"
        ),
        WorkspaceState::Removing => format!("shoal restore {id} reopens it once it is removed"),
        WorkspaceState::Ready
        | WorkspaceState::Stopping
        | WorkspaceState::Reconciling
        | WorkspaceState::Restoring
        | WorkspaceState::Updating => {
            "run the command again once that operation finishes".to_owned()
        }
    };
    let message = format!("workspace {number} ({name}) is {state}; {remedy}");
    Refusal::new(Reason::NotReady, message).into()
}

/// Serialize an optional value for a JSON text column.
pub fn json_text<T: Serialize>(value: Option<&T>) -> Result<Option<String>> {
    value
        .map(serde_json::to_string)
        .transpose()
        .map_err(Into::into)
}

/// The column value of a `--permission` narrowing: its JSON, or none for
/// one that narrows nothing.
pub fn narrowing_column(
    narrowing: &crate::config::permissions::PermissionsConfig,
) -> Result<Option<String>> {
    Ok((!narrowing.is_empty())
        .then(|| serde_json::to_string(narrowing))
        .transpose()?)
}

/// Parse an optional JSON text column, reporting failures as SQL conversions.
pub fn json_column<T: DeserializeOwned>(row: &Row<'_>, name: &str) -> rusqlite::Result<Option<T>> {
    let index = row.as_ref().column_index(name)?;
    row.get::<_, Option<String>>(index)?
        .map(|json| {
            serde_json::from_str(&json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

/// Whether the repository's removal has begun, which refuses new work in it.
pub fn repository_removing(db: &Connection, repository_id: &RepositoryId) -> Result<bool> {
    exists(
        db,
        "SELECT 1 FROM repository_removals WHERE repository_id=?1",
        [repository_id],
    )
}

pub const REPOSITORY_COLUMNS: &str =
    "id,path,source,last_used,name,workspaces_dir,contribution,shared_url";

pub fn repository(row: &Row<'_>) -> rusqlite::Result<Repository> {
    Ok(Repository {
        id: row.get("id")?,
        path: PathBuf::from(row.get::<_, String>("path")?),
        source: row.get("source")?,
        last_used: row.get("last_used")?,
        name: row.get("name")?,
        workspaces_dir: PathBuf::from(row.get::<_, String>("workspaces_dir")?),
        contribution: row.get("contribution")?,
        shared_url: row.get("shared_url")?,
    })
}

pub const WORKSPACE_COLUMNS: &str = "id,number,repository_id,name,mode,narrowing,path,branch,start_commit,made_for,state,error,base_commit,base_ref,git_dir,pending_plan,setup_skipped,disposable";

pub fn workspace(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    let mut workspace = Workspace::record(
        row.get("id")?,
        row.get("repository_id")?,
        row.get("name")?,
        PathBuf::from(row.get::<_, String>("path")?),
        head(row)?,
        row.get("state")?,
    );
    workspace.number = row.get("number")?;
    workspace.mode = row.get("mode")?;
    workspace.narrowing = json_column(row, "narrowing")?.unwrap_or_default();
    workspace.error = row.get("error")?;
    workspace.checkout = checkout(row)?;
    workspace.setup_skipped = row.get("setup_skipped")?;
    workspace.disposable = row.get("disposable")?;
    Ok(workspace)
}

/// A workspace's branch, or the commit it started at without one, from its
/// row, which holds exactly one of them.
fn head(row: &Row<'_>) -> rusqlite::Result<Head> {
    let conversion = |error: Box<dyn std::error::Error + Send + Sync>| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error)
    };
    let made_for = match row.get::<_, Option<String>>("made_for")? {
        Some(json) => Some(serde_json::from_str(&json).map_err(|error| conversion(error.into()))?),
        None => None,
    };
    match (
        row.get("branch")?,
        row.get::<_, Option<String>>("start_commit")?,
    ) {
        (Some(branch), None) if made_for.is_none() => Ok(Head::Branch(branch)),
        (None, Some(commit)) => Ok(Head::Commit(crate::model::StartingCommit {
            commit: commit.into(),
            made_for,
        })),
        _ => Err(conversion(
            "a workspace row holds exactly one of a branch and a starting commit".into(),
        )),
    }
}

/// What a workspace's row records of its worktree: pending while it holds
/// the plan of its creation, else created, which the table refuses without
/// a base and admin directory.
fn checkout(row: &Row<'_>) -> rusqlite::Result<Checkout> {
    let base = match row.get::<_, Option<CommitId>>("base_commit")? {
        Some(commit) => Some(Base {
            commit,
            reference: row.get("base_ref")?,
        }),
        None => None,
    };
    let git_dir = row.get::<_, Option<String>>("git_dir")?.map(PathBuf::from);
    match (row.get::<_, Option<String>>("pending_plan")?, base, git_dir) {
        (Some(_), base, git_dir) => Ok(Checkout::Pending { base, git_dir }),
        (None, Some(base), Some(git_dir)) => Ok(Checkout::Created { base, git_dir }),
        (None, ..) => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Null,
            "a created workspace's row holds its base and admin directory".into(),
        )),
    }
}

const PORT_COLUMNS: &str = "workspace_id,name,port,env_var,reason";

fn port(row: &Row<'_>) -> rusqlite::Result<PortReservation> {
    Ok(PortReservation {
        workspace_id: row.get("workspace_id")?,
        name: row.get("name")?,
        port: row.get("port")?,
        env_var: row.get("env_var")?,
        reason: row.get("reason")?,
    })
}

fn ports_query(by_owner: bool) -> String {
    let filter = if by_owner {
        "WHERE workspace_id=?1"
    } else {
        ""
    };
    format!("SELECT {PORT_COLUMNS} FROM ports {filter} ORDER BY workspace_id,name")
}

pub fn ports(db: &Connection, workspace_id: Option<&SpaceId>) -> Result<Vec<PortReservation>> {
    Ok(db
        .prepare(&ports_query(workspace_id.is_some()))?
        .query_map(rusqlite::params_from_iter(workspace_id), port)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub const EXECUTION_COLUMNS: &str =
    "id,space_id,state,wrapper,child,group_id,origin,agent,model,effort,mode,narrowing";

pub fn execution(row: &Row<'_>) -> rusqlite::Result<Execution> {
    Ok(Execution {
        id: row.get("id")?,
        space_id: row.get("space_id")?,
        state: row.get("state")?,
        wrapper: json_column(row, "wrapper")?,
        child: json_column(row, "child")?,
        group_id: row.get("group_id")?,
        origin: json_column(row, "origin")?,
        agent: row.get("agent")?,
        model: crate::agent::ModelChoice {
            model: row.get("model")?,
            effort: row.get("effort")?,
        },
        mode: row.get("mode")?,
        narrowing: json_column(row, "narrowing")?.unwrap_or_default(),
        stopped: None,
    })
}

fn executions_query() -> String {
    format!("SELECT {EXECUTION_COLUMNS} FROM executions WHERE space_id=?1")
}

pub fn find_execution(db: &Connection, id: &ExecutionId) -> Result<Option<Execution>> {
    Ok(db
        .query_row(
            &format!("SELECT {EXECUTION_COLUMNS} FROM executions WHERE id=?1"),
            [id],
            execution,
        )
        .optional()?)
}

/// The executions of the space `space_id`.
pub fn executions(db: &Connection, space_id: &SpaceId) -> Result<Vec<Execution>> {
    Ok(db
        .prepare(&executions_query())?
        .query_map([space_id], execution)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The wrappers of the recorded executions that run an agent.
pub fn agent_wrappers(db: &Connection) -> Result<Vec<crate::process::identity::Identity>> {
    let mut statement = db.prepare(
        "SELECT wrapper FROM executions WHERE agent IS NOT NULL AND wrapper IS NOT NULL",
    )?;
    let wrappers = statement.query_map([], |row| json_column(row, "wrapper"))?;
    Ok(wrappers
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

/// A recorded execution that runs an agent.
pub struct AgentExecution {
    pub agent: crate::agent::Agent,
    pub state: ExecutionState,
    /// Whether its wrapper runs the agent now, rather than waiting to start
    /// it or to restore one protection stopped.
    pub started: bool,
}

impl AgentExecution {
    /// How much the execution says about its space's agent: one that
    /// runs, then one not known to, then a wrapper without one.
    fn weight(&self) -> u8 {
        match (self.state, self.started) {
            (ExecutionState::Running, true) => 2,
            (ExecutionState::Unknown, _) => 1,
            (ExecutionState::Running, false) => 0,
        }
    }
}

/// The agent execution saying most about each space's agent, by space ID.
pub fn agent_executions(db: &mut Connection) -> Result<HashMap<SpaceId, AgentExecution>> {
    let mut agents: HashMap<SpaceId, AgentExecution> = HashMap::new();
    let mut statement = db.prepare(
        "SELECT space_id,agent,state,child IS NOT NULL FROM executions WHERE agent IS NOT NULL",
    )?;
    let rows = statement.query_map([], |row| {
        let execution = AgentExecution {
            agent: row.get(1)?,
            state: row.get(2)?,
            started: row.get(3)?,
        };
        Ok((row.get::<_, SpaceId>(0)?, execution))
    })?;
    for row in rows {
        let (space, execution) = row?;
        match agents.get(&space) {
            Some(known) if known.weight() >= execution.weight() => {}
            _ => {
                agents.insert(space, execution);
            }
        }
    }
    Ok(agents)
}

pub fn all_executions(db: &Connection) -> Result<Vec<Execution>> {
    Ok(db
        .prepare(&format!("SELECT {EXECUTION_COLUMNS} FROM executions"))?
        .query_map([], execution)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod query_tests;

#[cfg(test)]
use migrations::*;

#[cfg(test)]
mod tests;
