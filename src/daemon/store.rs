//! SQLite persistence: schema migrations, row mappers, and the small
//! guards every mutation shares.
use crate::refusal::{Reason, Refusal};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Params, Row};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::HashMap, path::PathBuf, time::Duration};

use crate::{
    model::{Execution, LinkedItem, PortReservation, Relation, Repository, SpaceLinks, Workspace},
    sim::audit::CleanRequestStatus,
    state::{ExecutionState, WorkspaceState},
};

#[cfg(test)]
mod benchmark;
mod chat_names;
#[cfg(test)]
mod event_tests;
mod item_numbers;
mod migrations;
mod place_servers;
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
        store
            .run(migrations::migrate)
            .await
            .context("migrate Shoal state database")?;
        Ok(store)
    }

    /// Quarantine interrupted operations while the daemon holds its startup lock.
    /// This commits separately from migration and must precede auditing or serving.
    pub async fn quarantine_interrupted_operations(&self) -> Result<()> {
        self.run(|db| {
            let tx = db.transaction()?;
            tx.execute(
                "UPDATE simulator_clean_requests SET record=json_set(record, '$.status', ?1)
                    WHERE json_extract(record, '$.status')=?2",
                [CleanRequestStatus::Interrupted, CleanRequestStatus::Requested],
            )?;
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
            tx.commit()?;
            Ok(())
        })
        .await
        .context("quarantine interrupted operations at daemon startup")
    }

    /// Run once on the connection's blocking worker, waiting for queue space.
    pub async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.worker.run(operation).await
    }

    pub(crate) fn watch_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.worker.changed.subscribe()
    }

    pub async fn shutdown(&self) {
        self.worker.shutdown().await;
    }
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

/// What holds a name: workspaces, unfinished renames and running chats share
/// one namespace across repositories.
enum NameHolder {
    Workspace,
    Rename,
    Chat,
}

/// What holds `name`, other than the workspace `except_id`. A chat holds
/// its name while it runs or waits saved, as far as the caller ended
/// finished ones first.
fn name_holder(db: &Connection, name: &str, except_id: Option<&str>) -> Result<Option<NameHolder>> {
    if exists(
        db,
        "SELECT 1 FROM workspaces WHERE name=?1 AND (?2 IS NULL OR id!=?2)",
        rusqlite::params![name, except_id],
    )? {
        return Ok(Some(NameHolder::Workspace));
    }
    if exists(db, "SELECT 1 FROM workspace_renames WHERE name=?1", [name])? {
        return Ok(Some(NameHolder::Rename));
    }
    if exists(db, "SELECT 1 FROM chats WHERE name=?1", [name])? {
        return Ok(Some(NameHolder::Chat));
    }
    Ok(None)
}

/// Refuse a name another workspace, an unfinished rename or a chat, running
/// or saved, holds.
pub fn ensure_name_available(db: &Connection, name: &str, except_id: Option<&str>) -> Result<()> {
    match name_holder(db, name, except_id)? {
        None => Ok(()),
        Some(NameHolder::Workspace) => Err(Refusal::new(
            Reason::NameTaken,
            format!("workspace name already exists: {name}"),
        )
        .into()),
        Some(NameHolder::Rename) => Err(Refusal::new(
            Reason::NameTaken,
            format!("workspace name is reserved by an unfinished rename: {name}"),
        )
        .into()),
        Some(NameHolder::Chat) => Err(Refusal::new(
            Reason::NameTaken,
            format!("a chat already has the name {name}"),
        )
        .into()),
    }
}

/// Whether a workspace, an unfinished rename or a chat, running or saved,
/// holds `name`.
pub fn name_taken(db: &Connection, name: &str) -> Result<bool> {
    Ok(name_holder(db, name, None)?.is_some())
}

/// The first of `base`, `base-2`, `base-3`, ... that nothing holds, each cut
/// to the longest name allowed.
pub fn free_name(db: &Connection, base: &str) -> Result<String> {
    for suffix in 1_u64.. {
        let candidate = crate::daemon::chats::suffixed(base, suffix);
        if !name_taken(db, &candidate)? {
            return Ok(candidate);
        }
    }
    unreachable!("an unused suffix exists")
}

/// Check ownership before inserting a workspace or reserving a rename in a transaction.
/// Names (including pending renames) are global; branches belong to one repository.
pub fn ensure_name_and_branch_available(
    db: &Connection,
    repository_id: &str,
    name: &str,
    branch: Option<&str>,
    except_id: Option<&str>,
) -> Result<()> {
    ensure_name_available(db, name, except_id)?;
    let Some(branch) = branch else {
        return Ok(());
    };
    let owner: Option<String> = db
        .query_row(
            "SELECT name FROM workspaces WHERE repository_id=?1 AND branch=?2
                AND (?3 IS NULL OR id!=?3)",
            rusqlite::params![repository_id, branch, except_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(owner) = owner {
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
pub fn ensure_no_pending_rename(db: &Connection, workspace_id: &str) -> Result<()> {
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
pub fn ensure_no_pending_move(db: &Connection, workspace_id: &str) -> Result<()> {
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
pub fn ensure_repository_not_moving(db: &Connection, repository_id: &str) -> Result<()> {
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

pub fn repository_has_workspaces(db: &Connection, repository_id: &str) -> Result<bool> {
    exists(db, REPOSITORY_WORKSPACES_QUERY, [repository_id])
}

/// Fail unless the workspace is ready; resources may only change then.
pub fn require_ready(db: &Connection, workspace_id: &str) -> Result<()> {
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
pub fn repository_has_executions(db: &Connection, repository_id: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM executions e JOIN workspaces w ON w.id=e.workspace_id WHERE w.repository_id=?1)",
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
pub fn repository_removing(db: &Connection, repository_id: &str) -> Result<bool> {
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

pub const WORKSPACE_COLUMNS: &str = "id,number,repository_id,name,title,mode,path,branch,start_commit,made_for,state,error,base_commit,base_ref,git_dir,git_dir_id,setup_skipped,disposable";

pub fn workspace(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        holds: Vec::new(),
        review: Vec::new(),
        latest_review: None,
        agent_state: None,
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
        title: row.get("title")?,
        mode: row.get("mode")?,
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
pub fn space_links(db: &Connection, space_id: &str) -> rusqlite::Result<SpaceLinks> {
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
    workspace_id: &str,
    relation: Relation,
) -> rusqlite::Result<Vec<LinkedItem>> {
    db.prepare(
        "SELECT kind,url,title FROM space_items WHERE space_id=?1 AND relation=?2
         ORDER BY created_at,rowid",
    )?
    .query_map(rusqlite::params![workspace_id, relation], |row| {
        Ok(LinkedItem {
            kind: row.get(0)?,
            url: row.get(1)?,
            title: row.get(2)?,
        })
    })?
    .collect()
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

fn ports_query(scoped: bool) -> String {
    let filter = if scoped { "WHERE workspace_id=?1" } else { "" };
    format!("SELECT {PORT_COLUMNS} FROM ports {filter} ORDER BY workspace_id,name")
}

pub fn ports(db: &Connection, workspace_id: Option<&str>) -> Result<Vec<PortReservation>> {
    Ok(db
        .prepare(&ports_query(workspace_id.is_some()))?
        .query_map(rusqlite::params_from_iter(workspace_id), port)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

const EXECUTION_COLUMNS: &str =
    "id,workspace_id,state,wrapper,child,group_id,origin,agent,model,effort,mode";

fn execution(row: &Row<'_>) -> rusqlite::Result<Execution> {
    Ok(Execution {
        id: row.get("id")?,
        workspace_id: row.get("workspace_id")?,
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
        stopped: None,
    })
}

fn executions_query() -> String {
    format!("SELECT {EXECUTION_COLUMNS} FROM executions WHERE workspace_id=?1")
}

pub fn find_execution(db: &Connection, id: &str) -> Result<Option<Execution>> {
    Ok(db
        .query_row(
            &format!("SELECT {EXECUTION_COLUMNS} FROM executions WHERE id=?1"),
            [id],
            execution,
        )
        .optional()?)
}

pub fn executions(db: &Connection, workspace_id: &str) -> Result<Vec<Execution>> {
    Ok(db
        .prepare(&executions_query())?
        .query_map([workspace_id], execution)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The agent execution `id` runs, when it runs one.
pub fn execution_agent(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT agent FROM executions WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?
        .flatten())
}

/// A recorded execution that runs an agent.
pub struct AgentExecution {
    pub agent: String,
    pub state: ExecutionState,
    /// Whether its wrapper runs the agent now, rather than waiting to start
    /// it or to restore one protection or a queue move stopped.
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
pub fn agent_executions(db: &mut Connection) -> Result<HashMap<String, AgentExecution>> {
    let mut agents: HashMap<String, AgentExecution> = HashMap::new();
    let mut statement = db.prepare(
        "SELECT workspace_id,agent,state,child IS NOT NULL FROM executions WHERE agent IS NOT NULL",
    )?;
    let rows = statement.query_map([], |row| {
        let execution = AgentExecution {
            agent: row.get(1)?,
            state: row.get(2)?,
            started: row.get(3)?,
        };
        Ok((row.get::<_, String>(0)?, execution))
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
pub fn repository_executions(db: &Connection, repository_id: &str) -> Result<Vec<Execution>> {
    Ok(db
        .prepare(&format!(
            "SELECT {EXECUTION_COLUMNS} FROM executions
                WHERE workspace_id IN (SELECT id FROM workspaces WHERE repository_id=?1)"
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

pub fn setup_finished(db: &Connection, workspace_id: &str) -> Result<bool> {
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
