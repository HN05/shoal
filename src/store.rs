use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Row};
use std::{path::PathBuf, time::Duration};

use crate::model::{Execution, Repository, Workspace};

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let store = Self { path };
        store.run(|db| {
            let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            ensure!(version <= 1, "state database was written by a newer Shoal version");
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
                    WHERE state IN ('preparing', 'removing', 'stopping');
                PRAGMA user_version=1;
                COMMIT;")?;
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
    })
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
