//! SQLite persistence: schema migrations, row mappers, and the small
//! guards every mutation shares.
use crate::client::execution_ids::ExecutionId;
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use crate::refusal::{Reason, Refusal};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Params, Row, Transaction, TransactionBehavior};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::HashMap, path::PathBuf, time::Duration};

use crate::{
    client::{sim::CleanRequestStatus, space_names::SpaceName},
    forge::items::reference::ItemRef,
    model::{Execution, LinkedItem, PortReservation, Relation, Repository, SpaceLinks, Workspace},
    state::{ExecutionState, WorkspaceState},
};

#[cfg(test)]
mod benchmark;
mod chat_names;
#[cfg(test)]
mod event_tests;
mod item_numbers;
mod migrations;
mod place_owners;
mod place_servers;
mod space_titles;
mod worker;
#[cfg(test)]
mod workspace_availability_tests;

#[derive(Clone)]
pub struct Store {
    worker: std::sync::Arc<worker::Worker>,
}

impl Store {
    /// Open persistence and migrate its schema without changing active operations.
    pub async fn open(path: PathBuf) -> Result<Self> {
        let store = Self {
            worker: worker::Worker::start(path)?,
        };
        // Not `write`: tests migrate plain connections, so the migration
        // opens its own transaction.
        store
            .run(migrations::migrate)
            .await
            .context("migrate Shoal state database")?;
        Ok(store)
    }

    /// Quarantine interrupted operations while the daemon holds its startup lock.
    /// This commits separately from migration and must precede auditing or serving.
    pub async fn quarantine_interrupted_operations(&self) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE simulator_clean_requests SET record=json_set(record, '$.status', ?1)
                    WHERE json_extract(record, '$.status')=?2",
                [CleanRequestStatus::Interrupted, CleanRequestStatus::Requested],
            )?;
            crate::daemon::workspace::fail_interrupted_removals(tx)?;
            tx.execute(
                "UPDATE executions SET state=?1 WHERE state=?2",
                [ExecutionState::Unknown, ExecutionState::Running],
            )?;
            tx.execute(
                "UPDATE workspaces SET state=?1, error='daemon stopped during workspace operation; inspect before cleanup'
                    WHERE state IN (SELECT value FROM json_each(?2))",
                rusqlite::params![
                    WorkspaceState::Failed,
                    serde_json::to_string(&WorkspaceState::transient().collect::<Vec<_>>())?,
                ],
            )?;
            Ok(())
        })
        .await
        .context("quarantine interrupted operations at daemon startup")
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

    pub(crate) fn watch_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.worker.changed.subscribe()
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
    Ok(db.query_row(&format!("SELECT EXISTS({sql})"), params, |row| row.get(0))?)
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

/// The first of `base`, `base-2`, `base-3`, ... that no workspace of any
/// repository has as its directory's name, each cut to the longest name
/// allowed.
pub fn free_directory(db: &Connection, base: &str) -> Result<String> {
    let recorded = db
        .prepare("SELECT path FROM workspaces")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let taken: std::collections::HashSet<&std::ffi::OsStr> = (recorded.iter())
        .filter_map(|path| std::path::Path::new(path).file_name())
        .collect();
    let free = (1_u64..)
        .map(|suffix| crate::client::space_names::suffixed_directory(base, suffix))
        .find(|candidate| !taken.contains(std::ffi::OsStr::new(candidate)));
    Ok(free.expect("an unused suffix exists"))
}

/// Refuse `path`, a directory Shoal named for a new workspace, when another
/// workspace's directory took its name since it was chosen, which a start in
/// another repository may have done meanwhile.
pub fn ensure_directory_free(db: &Connection, path: &std::path::Path) -> Result<()> {
    let name = path.file_name().unwrap_or_default();
    let taken = db
        .prepare("SELECT path FROM workspaces")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|recorded| std::path::Path::new(recorded).file_name() == Some(name));
    anyhow::ensure!(
        !taken,
        "another workspace took the directory name {} meanwhile; run the command again",
        name.to_string_lossy()
    );
    Ok(())
}

/// A recorded checkout or workspace that lies in `directory`.
pub fn checkout_within(
    db: &Connection,
    directory: &std::path::Path,
) -> Result<Option<std::path::PathBuf>> {
    let recorded = db
        .prepare("SELECT path FROM repositories UNION ALL SELECT path FROM workspaces")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(recorded
        .into_iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.starts_with(directory)))
}

/// Refuse a workspace path that overlaps where a repository directory is
/// moving: a failed move deletes its destination.
pub fn ensure_path_outside_moves(db: &Connection, path: &std::path::Path) -> Result<()> {
    let destinations = db
        .prepare("SELECT destination FROM repository_moves")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for destination in destinations.iter().map(std::path::Path::new) {
        ensure!(
            !path.starts_with(destination) && !destination.starts_with(path),
            "workspace path overlaps {}, where a repository directory is moving",
            destination.display()
        );
    }
    Ok(())
}

/// Refuse `branch` of the repository when another workspace than
/// `except_id` owns it, before inserting a workspace or reserving a rename
/// in a transaction.
pub fn ensure_branch_available(
    db: &Connection,
    repository_id: &RepositoryId,
    branch: Option<&str>,
    except_id: Option<&SpaceId>,
) -> Result<()> {
    let Some(branch) = branch else {
        return Ok(());
    };
    let owner: Option<(crate::client::space_ids::SpaceNumber, SpaceName)> = db
        .query_row(
            "SELECT number,name FROM workspaces WHERE repository_id=?1 AND branch=?2
                AND (?3 IS NULL OR id!=?3)",
            rusqlite::params![repository_id, branch, except_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((number, name)) = owner {
        let owner = crate::client::space_names::label(number, &name);
        return Err(Refusal::new(
            Reason::NameTaken,
            format!("branch is already owned by workspace {owner}"),
        )
        .into());
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
            "SELECT 1 FROM repository_moves JOIN workspaces USING (repository_id)
                WHERE workspaces.id=?1",
            [workspace_id]
        )?,
        "repository move is incomplete; run shoal move root again"
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
        "repository move is incomplete; run shoal move root again"
    );
    Ok(())
}

const REPOSITORY_WORKSPACES_QUERY: &str = "SELECT 1 FROM workspaces WHERE repository_id=?1";

pub fn repository_has_workspaces(db: &Connection, repository_id: &RepositoryId) -> Result<bool> {
    exists(db, REPOSITORY_WORKSPACES_QUERY, [repository_id])
}

/// Fail unless the workspace is ready; resources may only change then.
pub fn require_ready(db: &Connection, workspace_id: &SpaceId) -> Result<()> {
    let ready: bool = db.query_row(
        "SELECT state=?2 FROM workspaces WHERE id=?1",
        rusqlite::params![workspace_id, WorkspaceState::Ready],
        |row| row.get(0),
    )?;
    ensure!(
        ready,
        Refusal::new(Reason::NotReady, "workspace is not ready")
    );
    Ok(())
}

/// Whether any workspace of the repository has a tracked execution.
pub fn repository_has_executions(db: &Connection, repository_id: &RepositoryId) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM executions e JOIN workspaces w ON w.id=e.space_id WHERE w.repository_id=?1)",
        [repository_id],
        |row| row.get(0),
    )?)
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
pub(crate) fn narrowing_column(
    narrowing: &crate::config::permissions::PermissionsConfig,
) -> Result<Option<String>> {
    Ok((!narrowing.is_empty())
        .then(|| serde_json::to_string(narrowing))
        .transpose()?)
}

/// Parse an optional JSON text column, reporting failures as SQL conversions.
pub(crate) fn json_column<T: DeserializeOwned>(
    row: &Row<'_>,
    name: &str,
) -> rusqlite::Result<Option<T>> {
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

pub const REPOSITORY_COLUMNS: &str = "id,path,source,last_used,name,workspaces_dir,contribution";

pub fn repository(row: &Row<'_>) -> rusqlite::Result<Repository> {
    Ok(Repository {
        id: row.get("id")?,
        path: PathBuf::from(row.get::<_, String>("path")?),
        source: row.get("source")?,
        last_used: row.get("last_used")?,
        name: row.get("name")?,
        workspaces_dir: row
            .get::<_, Option<String>>("workspaces_dir")?
            .map(PathBuf::from),
        contribution: row.get("contribution")?,
    })
}

pub const WORKSPACE_COLUMNS: &str = "id,number,repository_id,name,mode,narrowing,path,branch,start_commit,made_for,state,error,base_commit,base_ref,git_dir,git_dir_id,setup_skipped,disposable";

pub fn workspace(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        holds: Vec::new(),
        review: Vec::new(),
        latest_review: None,
        agent_state: None,
        progress: None,
        sleep: None,
        coordinator_state: None,
        base_workspace: None,
        stacked_workspaces: Vec::new(),
        parent: None,
        children: Vec::new(),
        links: SpaceLinks::default(),
        running: false,
        done: false,
        usage: None,
        job: None,
        setup_skipped: row.get("setup_skipped")?,
        disposable: row.get("disposable")?,
        id: row.get("id")?,
        number: row.get("number")?,
        repository_id: row.get("repository_id")?,
        name: row.get("name")?,
        mode: row.get("mode")?,
        narrowing: json_column(row, "narrowing")?.unwrap_or_default(),
        path: PathBuf::from(row.get::<_, String>("path")?),
        branch: row.get("branch")?,
        at: starting_commit(row)?,
        copied_from: None,
        state: row.get("state")?,
        error: row.get("error")?,
        base_commit: row.get("base_commit")?,
        base_ref: row.get("base_ref")?,
        git_dir: row.get::<_, Option<String>>("git_dir")?.map(PathBuf::from),
        git_dir_id: row.get("git_dir_id")?,
    })
}

/// Where a workspace without a branch started, from its row.
fn starting_commit(row: &Row<'_>) -> rusqlite::Result<Option<crate::model::StartingCommit>> {
    let Some(commit) = row.get::<_, Option<String>>("start_commit")? else {
        return Ok(None);
    };
    let made_for = match row.get::<_, Option<String>>("made_for")? {
        Some(json) => Some(serde_json::from_str(&json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
        })?),
        None => None,
    };
    Ok(Some(crate::model::StartingCommit { commit, made_for }))
}

/// Read canonical links while keeping malformed legacy PR records out of list output.
pub fn space_links(db: &Connection, space_id: &SpaceId) -> rusqlite::Result<SpaceLinks> {
    let (issue, issue_title) = db
        .query_row(
            "SELECT url,title FROM space_issue WHERE space_id=?1",
            [space_id],
            |row| Ok((Some(row.get(0)?), row.get(1)?)),
        )
        .optional()?
        .unwrap_or_default();
    let prs = db
        .query_row(
            "SELECT record FROM pr_cleanup WHERE space_id=?1",
            [space_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|record| {
            serde_json::from_str::<crate::daemon::links::pr::Registration>(&record)
                .ok()
                .and_then(|registration| match registration.kind {
                    crate::daemon::links::pr::RegistrationKind::Watch { urls, .. } => Some(urls),
                    crate::daemon::links::pr::RegistrationKind::Acknowledgement { .. } => None,
                })
        })
        .unwrap_or_default();
    Ok(SpaceLinks {
        issue,
        issue_title,
        prs,
        opened: recorded_items(db, space_id, Relation::Opened)?,
        related: recorded_items(db, space_id, Relation::Related)?,
    })
}

/// The workspace's items with a recorded relation, oldest first.
pub fn recorded_items(
    db: &Connection,
    workspace_id: &SpaceId,
    relation: Relation,
) -> rusqlite::Result<Vec<LinkedItem>> {
    db.prepare(
        "SELECT kind,url,title FROM space_items WHERE space_id=?1 AND relation=?2
         ORDER BY created_at,rowid",
    )?
    .query_map(rusqlite::params![workspace_id, relation], |row| {
        Ok(LinkedItem {
            url: linked_url(row)?,
            title: row.get(2)?,
        })
    })?
    .collect()
}

/// The URL of a linked item's row, read as the kind the row records.
fn linked_url(row: &Row<'_>) -> rusqlite::Result<ItemRef> {
    let url: String = row.get(1)?;
    ItemRef::parse_kind(row.get(0)?, &url).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, error.into())
    })
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

const EXECUTION_COLUMNS: &str =
    "id,space_id,state,wrapper,child,group_id,origin,agent,model,effort,mode,narrowing";

fn execution(row: &Row<'_>) -> rusqlite::Result<Execution> {
    Ok(Execution {
        id: row.get("id")?,
        workspace_id: row.get("space_id")?,
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

pub fn executions(db: &Connection, workspace_id: &SpaceId) -> Result<Vec<Execution>> {
    Ok(db
        .prepare(&executions_query())?
        .query_map([workspace_id], execution)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The agent execution `id` runs, when it runs one.
pub fn execution_agent(db: &Connection, id: &ExecutionId) -> Result<Option<crate::agent::Agent>> {
    Ok(db
        .query_row("SELECT agent FROM executions WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?
        .flatten())
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
    /// How much the execution says about its workspace's agent: one that
    /// runs, then one not known to, then a wrapper without one.
    fn weight(&self) -> u8 {
        match (self.state, self.started) {
            (ExecutionState::Running, true) => 2,
            (ExecutionState::Unknown, _) => 1,
            (ExecutionState::Running, false) => 0,
        }
    }
}

/// The agent execution saying most about each workspace's agent, by
/// workspace ID.
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
        let (workspace, execution) = row?;
        match agents.get(&workspace) {
            Some(known) if known.weight() >= execution.weight() => {}
            _ => {
                agents.insert(workspace, execution);
            }
        }
    }
    Ok(agents)
}

/// The executions running in the workspaces of repository `repository_id`.
pub fn repository_executions(
    db: &Connection,
    repository_id: &RepositoryId,
) -> Result<Vec<Execution>> {
    Ok(db
        .prepare(&format!(
            "SELECT {EXECUTION_COLUMNS} FROM executions
                WHERE space_id IN (SELECT id FROM workspaces WHERE repository_id=?1)"
        ))?
        .query_map([repository_id], execution)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn all_executions(db: &Connection) -> Result<Vec<Execution>> {
    Ok(db
        .prepare(&format!("SELECT {EXECUTION_COLUMNS} FROM executions"))?
        .query_map([], execution)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn setup_finished(db: &Connection, workspace_id: &SpaceId) -> Result<bool> {
    Ok(db.query_row(
        "SELECT setup_finished FROM workspaces WHERE id=?1",
        [workspace_id],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
pub(super) mod query_tests;

#[cfg(test)]
use migrations::*;

#[cfg(test)]
mod tests;
