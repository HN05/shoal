use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Row};
use std::{path::PathBuf, time::Duration};

use crate::model::{Execution, PortReservation, Repository, Workspace};

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let store = Self { path };
        store.run(|db| {
            let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            ensure!(version <= 6, "state database was written by a newer Shoal version");
            db.execute_batch("BEGIN;
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
                );
                UPDATE executions SET state='unknown' WHERE state='running';
                UPDATE workspaces SET state='failed', error='daemon stopped during workspace operation; inspect before cleanup'
                    WHERE state IN ('preparing', 'removing', 'stopping');")?;
            if version < 2 {
                db.execute_batch("ALTER TABLE workspaces ADD COLUMN base_commit TEXT;
                    ALTER TABLE workspaces ADD COLUMN base_ref TEXT;")?;
            }
            db.execute_batch("CREATE TABLE IF NOT EXISTS ports (
                workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                name TEXT NOT NULL, port INTEGER NOT NULL UNIQUE CHECK(port BETWEEN 1 AND 65535),
                env_var TEXT NOT NULL, PRIMARY KEY(workspace_id, name), UNIQUE(workspace_id, env_var)
            );")?;
            if version < 4 { db.execute_batch("ALTER TABLE repositories ADD COLUMN name TEXT;")?; }
            if version < 5 { db.execute_batch("ALTER TABLE ports ADD COLUMN reason TEXT;")?; }
            db.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS repository_names ON repositories(name) WHERE name IS NOT NULL;
                CREATE TABLE IF NOT EXISTS simulators(id TEXT PRIMARY KEY, record TEXT NOT NULL);
                PRAGMA user_version=6; COMMIT;")?;
            Ok(())
        }).await?;
        Ok(store)
    }

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

pub fn repository(row: &Row<'_>) -> rusqlite::Result<Repository> {
    Ok(Repository {
        id: row.get(0)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        source: row.get(2)?,
        last_used: row.get(3)?,
        name: row.get(4)?,
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
    })
}

pub fn ports(db: &Connection, workspace_id: Option<&str>) -> Result<Vec<PortReservation>> {
    Ok(db.prepare("SELECT workspace_id,name,port,env_var,reason FROM ports WHERE ?1 IS NULL OR workspace_id=?1 ORDER BY workspace_id,name")?
        .query_map([workspace_id], |row| Ok(PortReservation {
            workspace_id: row.get(0)?, name: row.get(1)?, port: row.get(2)?, env_var: row.get(3)?,
            reason: row.get(4)?,
        }))?.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn executions(db: &Connection, workspace_id: &str) -> Result<Vec<Execution>> {
    Ok(db
        .prepare("SELECT id, workspace_id, state FROM executions WHERE workspace_id=?1")?
        .query_map([workspace_id], |r| {
            Ok(Execution {
                id: r.get(0)?,
                workspace_id: r.get(1)?,
                state: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
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
                assert!(repo.name.is_none());
                let workspace = db.query_row("SELECT * FROM workspaces", [], workspace)?;
                assert_eq!(workspace.name, "feature");
                assert!(workspace.base_commit.is_none() && workspace.base_ref.is_none());
                assert_eq!(executions(db, "workspace")?[0].state, "unknown");
                assert!(ports(db, None)?.is_empty());
                Ok(())
            })
            .await
            .unwrap();
        Store::open(path).await.unwrap();
    }
}
