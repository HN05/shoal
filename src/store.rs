//! SQLite persistence: schema migrations, row mappers, and the small
//! guards every mutation shares.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Row};
use serde::{Serialize, de::DeserializeOwned};
use std::{path::PathBuf, time::Duration};

use crate::{
    model::{Execution, PortReservation, Repository, Workspace},
    state::WorkspaceState,
};

/// Schema version written by this build; older databases are migrated on open.
const SCHEMA_VERSION: i64 = 16;

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let store = Self { path };
        store.run(migrate).await?;
        Ok(store)
    }

    /// Run `operation` on a fresh connection on the blocking pool.
    pub async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut db = Connection::open(path).context("open Shoal state database")?;
            db.busy_timeout(Duration::from_secs(5))?;
            db.execute_batch("PRAGMA foreign_keys=ON;")?;
            operation(&mut db)
        })
        .await
        .context("database worker failed")?
    }
}

fn migrate(db: &mut Connection) -> Result<()> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    ensure!(
        version <= SCHEMA_VERSION,
        "state database was written by a newer Shoal version"
    );
    // Interrupted operations are quarantined rather than assumed complete.
    db.execute_batch(
        "BEGIN;
        CREATE TABLE IF NOT EXISTS repositories (
            id TEXT PRIMARY KEY, path TEXT NOT NULL UNIQUE, source TEXT NOT NULL, last_used INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS workspaces (
            id TEXT PRIMARY KEY, repository_id TEXT NOT NULL REFERENCES repositories(id),
            name TEXT NOT NULL UNIQUE, path TEXT NOT NULL UNIQUE, branch TEXT NOT NULL,
            state TEXT NOT NULL, error TEXT
        );
        CREATE TABLE IF NOT EXISTS executions (
            id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id), state TEXT NOT NULL
        );",
    )?;
    if version < 2 {
        db.execute_batch(
            "ALTER TABLE workspaces ADD COLUMN base_commit TEXT;
            ALTER TABLE workspaces ADD COLUMN base_ref TEXT;",
        )?;
    }
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS ports (
            workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            name TEXT NOT NULL, port INTEGER NOT NULL UNIQUE CHECK(port BETWEEN 1 AND 65535),
            env_var TEXT NOT NULL, PRIMARY KEY(workspace_id, name), UNIQUE(workspace_id, env_var)
        );",
    )?;
    if version < 4 {
        db.execute_batch("ALTER TABLE repositories ADD COLUMN name TEXT;")?;
    }
    if version < 5 {
        db.execute_batch("ALTER TABLE ports ADD COLUMN reason TEXT;")?;
    }
    db.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS repository_names ON repositories(name) WHERE name IS NOT NULL;
        CREATE TABLE IF NOT EXISTS simulators(id TEXT PRIMARY KEY, record TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS simulator_clean_requests (
            id INTEGER PRIMARY KEY AUTOINCREMENT, request_id TEXT NOT NULL UNIQUE,
            workspace_id TEXT NOT NULL, record TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS clean_requests_workspace ON simulator_clean_requests(workspace_id,id);
        UPDATE simulator_clean_requests SET record=json_set(record, '$.status', 'interrupted') WHERE json_extract(record, '$.status')='requested';
        CREATE TABLE IF NOT EXISTS resource_pools (
            scope TEXT NOT NULL, name TEXT NOT NULL, definition TEXT NOT NULL, PRIMARY KEY(scope,name)
        );
        CREATE TABLE IF NOT EXISTS resource_leases (
            id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            scope TEXT NOT NULL, pool TEXT NOT NULL, name TEXT NOT NULL, resource TEXT NOT NULL,
            reason TEXT, created_at INTEGER NOT NULL, UNIQUE(workspace_id,pool,name),
            FOREIGN KEY(scope,pool) REFERENCES resource_pools(scope,name)
        );
        CREATE INDEX IF NOT EXISTS resource_lease_pool ON resource_leases(scope,pool);",
    )?;
    if version < 9 {
        db.execute_batch(
            "ALTER TABLE resource_leases ADD COLUMN mode TEXT NOT NULL DEFAULT 'permit' CHECK(mode IN ('permit','read','write'));",
        )?;
    }
    if version < 10 {
        db.execute_batch(
            "ALTER TABLE workspaces ADD COLUMN git_dir TEXT;
            ALTER TABLE workspaces ADD COLUMN git_dir_id TEXT;
            ALTER TABLE executions ADD COLUMN wrapper TEXT;
            ALTER TABLE executions ADD COLUMN child TEXT;
            ALTER TABLE executions ADD COLUMN group_id INTEGER;",
        )?;
    }
    if version < 13 {
        db.execute_batch("ALTER TABLE repositories ADD COLUMN workspaces_dir TEXT;")?;
    }
    if version < 16 {
        db.execute_batch(
            "ALTER TABLE workspaces ADD COLUMN setup_finished INTEGER NOT NULL DEFAULT 0 CHECK(setup_finished IN (0,1));
            UPDATE workspaces SET setup_finished=CASE
                WHEN state='preparing' OR error LIKE 'setup failed (%' THEN 0
                ELSE 1
            END;",
        )?;
    }
    db.execute_batch(
        "UPDATE executions SET state='unknown' WHERE state='running';
        UPDATE workspaces SET state='failed', error='daemon stopped during workspace operation; inspect before cleanup'
            WHERE state IN ('preparing', 'removing', 'stopping', 'reconciling');",
    )?;
    db.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS repository_removals (
            repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
            directory_id TEXT,
            deleting_files INTEGER NOT NULL DEFAULT 0 CHECK(deleting_files IN (0,1))
        );
        CREATE TABLE IF NOT EXISTS repository_configs (
            repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
            toml TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS pr_cleanup (
            workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
            record TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS notifications (
            id INTEGER PRIMARY KEY AUTOINCREMENT, created_at INTEGER NOT NULL, workspace TEXT,
            kind TEXT NOT NULL, message TEXT NOT NULL, read INTEGER NOT NULL DEFAULT 0 CHECK(read IN (0,1))
        );
        CREATE INDEX IF NOT EXISTS notifications_unread ON notifications(read,id);
        PRAGMA user_version={SCHEMA_VERSION};
        COMMIT;"
    ))?;
    Ok(())
}

/// Fail unless the workspace is ready; resources may only change then.
pub fn require_ready(db: &Connection, workspace_id: &str) -> Result<()> {
    let ready: bool = db.query_row(
        "SELECT state=?2 FROM workspaces WHERE id=?1",
        rusqlite::params![workspace_id, WorkspaceState::Ready],
        |row| row.get(0),
    )?;
    ensure!(ready, "workspace is not ready");
    Ok(())
}

/// Serialize an optional value for a JSON text column.
pub fn json_text<T: Serialize>(value: Option<&T>) -> Result<Option<String>> {
    value
        .map(serde_json::to_string)
        .transpose()
        .map_err(Into::into)
}

/// Parse an optional JSON text column, reporting failures as SQL conversions.
fn json_column<T: DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<T>> {
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

pub fn repository(row: &Row<'_>) -> rusqlite::Result<Repository> {
    Ok(Repository {
        id: row.get(0)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        source: row.get(2)?,
        last_used: row.get(3)?,
        name: row.get(4)?,
        workspaces_dir: row.get::<_, Option<String>>(5)?.map(PathBuf::from),
    })
}

pub fn workspace(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: row.get(0)?,
        repository_id: row.get(1)?,
        name: row.get(2)?,
        path: PathBuf::from(row.get::<_, String>(3)?),
        branch: row.get(4)?,
        state: row.get(5)?,
        error: row.get(6)?,
        base_commit: row.get(7)?,
        base_ref: row.get(8)?,
        git_dir: row.get::<_, Option<String>>(9)?.map(PathBuf::from),
        git_dir_id: row.get(10)?,
    })
}

pub fn ports(db: &Connection, workspace_id: Option<&str>) -> Result<Vec<PortReservation>> {
    Ok(db
        .prepare(
            "SELECT workspace_id,name,port,env_var,reason FROM ports WHERE ?1 IS NULL OR workspace_id=?1 ORDER BY workspace_id,name",
        )?
        .query_map([workspace_id], |row| {
            Ok(PortReservation {
                workspace_id: row.get(0)?,
                name: row.get(1)?,
                port: row.get(2)?,
                env_var: row.get(3)?,
                reason: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn executions(db: &Connection, workspace_id: &str) -> Result<Vec<Execution>> {
    Ok(db
        .prepare(
            "SELECT id, workspace_id, state, wrapper, child, group_id FROM executions WHERE workspace_id=?1",
        )?
        .query_map([workspace_id], |row| {
            Ok(Execution {
                id: row.get(0)?,
                workspace_id: row.get(1)?,
                state: row.get(2)?,
                wrapper: json_column(row, 3)?,
                child: json_column(row, 4)?,
                group_id: row.get(5)?,
            })
        })?
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrates_original_database_without_losing_ownership_records() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE repositories(id TEXT PRIMARY KEY,path TEXT UNIQUE,source TEXT,last_used INTEGER);
            CREATE TABLE workspaces(id TEXT PRIMARY KEY,repository_id TEXT,name TEXT UNIQUE,path TEXT UNIQUE,branch TEXT,state TEXT,error TEXT);
            CREATE TABLE executions(id TEXT PRIMARY KEY,workspace_id TEXT,state TEXT);
            INSERT INTO repositories VALUES ('repo','/repo','/repo',1);
            INSERT INTO workspaces VALUES ('workspace','repo','feature','/work','feature','ready',NULL);
            INSERT INTO executions VALUES ('execution','workspace','running');
            PRAGMA user_version=1;").unwrap();
        drop(db);
        let store = Store::open(path.clone()).await.unwrap();
        store
            .run(|db| {
                let repo = db.query_row("SELECT * FROM repositories", [], repository)?;
                assert_eq!(repo.id, "repo");
                assert!(repo.name.is_none() && repo.workspaces_dir.is_none());
                let workspace = db.query_row("SELECT * FROM workspaces", [], workspace)?;
                assert_eq!(workspace.name, "feature");
                assert!(workspace.base_commit.is_none() && workspace.base_ref.is_none());
                assert!(setup_finished(db, "workspace")?);
                assert_eq!(
                    executions(db, "workspace")?[0].state,
                    crate::state::ExecutionState::Unknown
                );
                assert!(ports(db, None)?.is_empty());
                Ok(())
            })
            .await
            .unwrap();
        Store::open(path).await.unwrap();
    }

    #[tokio::test]
    async fn migrates_semaphore_leases_and_definitions_from_version_eight() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = Store::open(path.clone()).await.unwrap();
        store.run(|db| {
            db.execute_batch("INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES ('workspace','repo','worker','/work','worker','ready');
                ALTER TABLE resource_leases DROP COLUMN mode;
                ALTER TABLE workspaces DROP COLUMN git_dir;
                ALTER TABLE workspaces DROP COLUMN git_dir_id;
                ALTER TABLE workspaces DROP COLUMN setup_finished;
                ALTER TABLE executions DROP COLUMN wrapper;
                ALTER TABLE executions DROP COLUMN child;
                ALTER TABLE executions DROP COLUMN group_id;
                ALTER TABLE repositories DROP COLUMN workspaces_dir;
                PRAGMA user_version=8;")?;
            let definition = r#"{"capacity":2,"reason":null,"resources":{"worker":{"capacity":2,"reason":null}}}"#;
            db.execute("INSERT INTO resource_pools(scope,name,definition) VALUES ('global','worker',?1)", [definition])?;
            db.execute("INSERT INTO resource_leases VALUES ('lease','workspace','global','worker','default','worker',NULL,123)", [])?;
            Ok(())
        }).await.unwrap();
        let migrated = Store::open(path.clone()).await.unwrap();
        migrated.run(|db| {
            let leases = crate::resources::leases(db, None)?;
            assert_eq!(leases.len(), 1);
            assert_eq!(leases[0].id, "lease");
            assert_eq!(leases[0].mode, crate::resources::LockMode::Permit);
            assert_eq!(leases[0].created_at, 123);
            let json: String = db.query_row("SELECT definition FROM resource_pools", [], |row| row.get(0))?;
            let stored: crate::resources::Definition = serde_json::from_str(&json)?;
            let config: crate::repo_config::RepoConfig = toml::from_str("[resources.worker]\ncapacity=2")?;
            assert_eq!(stored, crate::resources::definitions(&config.resources, &config.resource_pools)?["worker"]);
            assert!(db.execute("UPDATE resource_leases SET mode='invalid'", []).is_err());
            assert!(db.query_row("SELECT 'invalid'", [], |row| row.get::<_, crate::resources::LockMode>(0)).is_err());
            Ok(())
        }).await.unwrap();
        Store::open(path).await.unwrap();
    }

    #[tokio::test]
    async fn migrates_setup_completion_without_losing_interrupted_state() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = Store::open(path.clone()).await.unwrap();
        store
            .run(|db| {
                db.execute_batch(
                    "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                    INSERT INTO workspaces(id,repository_id,name,path,branch,state,error) VALUES
                        ('ready','repo','ready','/ready','ready','ready',NULL),
                        ('preparing','repo','preparing','/preparing','preparing','preparing',NULL),
                        ('failed','repo','failed','/failed','failed','failed','setup failed (exit 1, processes stopped: true); retry with shoal prepare');
                    ALTER TABLE workspaces DROP COLUMN setup_finished;
                    PRAGMA user_version=15;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let migrated = Store::open(path).await.unwrap();
        migrated
            .run(|db| {
                assert!(setup_finished(db, "ready")?);
                assert!(!setup_finished(db, "preparing")?);
                assert!(!setup_finished(db, "failed")?);
                let state: WorkspaceState = db.query_row(
                    "SELECT state FROM workspaces WHERE id='preparing'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(state, WorkspaceState::Failed);
                Ok(())
            })
            .await
            .unwrap();
    }
}
