//! SQLite persistence: schema migrations, row mappers, and the small
//! guards every mutation shares.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Params, Row};
use serde::{Serialize, de::DeserializeOwned};
use std::{path::PathBuf, time::Duration};

use crate::{
    model::{Execution, PortReservation, Repository, Workspace},
    sim::audit::CleanRequestStatus,
    state::{ExecutionState, WorkspaceState},
};

/// Schema version written by this build; older databases are migrated on open.
const SCHEMA_VERSION: i64 = 19;

#[cfg(test)]
mod benchmark;

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// Open persistence and migrate its schema without changing active operations.
    pub async fn open(path: PathBuf) -> Result<Self> {
        let store = Self { path };
        store
            .run(migrate)
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
                    WHERE state IN (?2, ?3, ?4, ?5)",
                [
                    WorkspaceState::Failed,
                    WorkspaceState::Preparing,
                    WorkspaceState::Removing,
                    WorkspaceState::Stopping,
                    WorkspaceState::Reconciling,
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .context("quarantine interrupted operations at daemon startup")
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

/// Checks existing rows before a migration's SQL runs; failure aborts the upgrade.
type Precondition = fn(&Connection) -> Result<()>;

// Append schema changes in version order; startup recovery belongs in quarantine.
const MIGRATIONS: &[(i64, &str, Option<Precondition>)] = &[
    (
        1,
        "CREATE TABLE IF NOT EXISTS repositories (
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
        None,
    ),
    (
        2,
        "ALTER TABLE workspaces ADD COLUMN base_commit TEXT;
        ALTER TABLE workspaces ADD COLUMN base_ref TEXT;",
        None,
    ),
    (
        3,
        "CREATE TABLE IF NOT EXISTS ports (
            workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            name TEXT NOT NULL, port INTEGER NOT NULL UNIQUE CHECK(port BETWEEN 1 AND 65535),
            env_var TEXT NOT NULL, PRIMARY KEY(workspace_id, name), UNIQUE(workspace_id, env_var)
        );",
        None,
    ),
    (
        4,
        "ALTER TABLE repositories ADD COLUMN name TEXT;
        CREATE UNIQUE INDEX IF NOT EXISTS repository_names ON repositories(name) WHERE name IS NOT NULL;",
        None,
    ),
    (
        5,
        "ALTER TABLE ports ADD COLUMN reason TEXT;",
        None,
    ),
    (
        6,
        "CREATE TABLE IF NOT EXISTS simulators(id TEXT PRIMARY KEY, record TEXT NOT NULL);",
        None,
    ),
    (
        7,
        "CREATE TABLE IF NOT EXISTS simulator_clean_requests (
            id INTEGER PRIMARY KEY AUTOINCREMENT, request_id TEXT NOT NULL UNIQUE,
            workspace_id TEXT NOT NULL, record TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS clean_requests_workspace ON simulator_clean_requests(workspace_id,id);",
        None,
    ),
    (
        8,
        "CREATE TABLE IF NOT EXISTS resource_pools (
            scope TEXT NOT NULL, name TEXT NOT NULL, definition TEXT NOT NULL, PRIMARY KEY(scope,name)
        );
        CREATE TABLE IF NOT EXISTS resource_leases (
            id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            scope TEXT NOT NULL, pool TEXT NOT NULL, name TEXT NOT NULL, resource TEXT NOT NULL,
            reason TEXT, created_at INTEGER NOT NULL, UNIQUE(workspace_id,pool,name),
            FOREIGN KEY(scope,pool) REFERENCES resource_pools(scope,name)
        );
        CREATE INDEX IF NOT EXISTS resource_lease_pool ON resource_leases(scope,pool);",
        None,
    ),
    (
        9,
        "ALTER TABLE resource_leases ADD COLUMN mode TEXT NOT NULL DEFAULT 'permit' CHECK(mode IN ('permit','read','write'));",
        None,
    ),
    (
        10,
        "ALTER TABLE workspaces ADD COLUMN git_dir TEXT;
        ALTER TABLE workspaces ADD COLUMN git_dir_id TEXT;
        ALTER TABLE executions ADD COLUMN wrapper TEXT;
        ALTER TABLE executions ADD COLUMN child TEXT;
        ALTER TABLE executions ADD COLUMN group_id INTEGER;",
        None,
    ),
    (
        11,
        "CREATE TABLE IF NOT EXISTS repository_removals (
            repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
            directory_id TEXT,
            deleting_files INTEGER NOT NULL DEFAULT 0 CHECK(deleting_files IN (0,1))
        );",
        None,
    ),
    (
        12,
        "CREATE TABLE IF NOT EXISTS repository_configs (
            repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
            toml TEXT NOT NULL
        );",
        None,
    ),
    (
        13,
        "ALTER TABLE repositories ADD COLUMN workspaces_dir TEXT;",
        None,
    ),
    (
        14,
        "CREATE TABLE IF NOT EXISTS pr_cleanup (
            workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
            record TEXT NOT NULL
        );",
        None,
    ),
    (
        15,
        "CREATE TABLE IF NOT EXISTS notifications (
            id INTEGER PRIMARY KEY AUTOINCREMENT, created_at INTEGER NOT NULL, workspace TEXT,
            kind TEXT NOT NULL, message TEXT NOT NULL, read INTEGER NOT NULL DEFAULT 0 CHECK(read IN (0,1))
        );
        CREATE INDEX IF NOT EXISTS notifications_unread ON notifications(read,id);",
        None,
    ),
    (
        16,
        "ALTER TABLE workspaces ADD COLUMN setup_finished INTEGER NOT NULL DEFAULT 0 CHECK(setup_finished IN (0,1));
        UPDATE workspaces SET setup_finished=CASE
                WHEN state='preparing' OR error LIKE 'setup failed (%' THEN 0
                ELSE 1
            END;",
        None,
    ),
    (
        17,
        "CREATE TABLE IF NOT EXISTS access_requests (
            id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            target_key TEXT NOT NULL, name TEXT NOT NULL,
            record TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1))
        );
        CREATE UNIQUE INDEX IF NOT EXISTS access_request_name
            ON access_requests(workspace_id,target_key,name) WHERE active=1;",
        None,
    ),
    (
        18,
        "CREATE INDEX IF NOT EXISTS executions_workspace ON executions(workspace_id);
        CREATE INDEX IF NOT EXISTS workspaces_repository ON workspaces(repository_id);",
        None,
    ),
    (
        19,
        include_str!("store/simulator_owner.sql"),
        Some(validate_simulator_records),
    ),
    (
        19,
        "CREATE INDEX IF NOT EXISTS access_request_target
            ON access_requests(workspace_id,target_key);",
    ),
];

fn migrate(db: &mut Connection) -> Result<()> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    ensure!(
        version <= SCHEMA_VERSION,
        "state database was written by a newer Shoal version"
    );
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    let tx = db.transaction()?;
    for &(target, sql, precondition) in MIGRATIONS {
        if target > version {
            if let Some(precondition) = precondition {
                precondition(&tx)?;
            }
            tx.execute_batch(sql)
                .with_context(|| format!("apply schema migration {target}"))?;
            tx.pragma_update(None, "user_version", target)?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn validate_simulator_records(db: &Connection) -> Result<()> {
    crate::sim::records::list(db, None)
        .context("validate simulator records before indexing ownership")?;
    Ok(())
}

/// Whether the supplied SELECT query returns any rows.
pub fn exists(db: &Connection, sql: &str, params: impl Params) -> Result<bool> {
    Ok(db.query_row(&format!("SELECT EXISTS({sql})"), params, |row| row.get(0))?)
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
fn json_column<T: DeserializeOwned>(row: &Row<'_>, name: &str) -> rusqlite::Result<Option<T>> {
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

pub const REPOSITORY_COLUMNS: &str = "id,path,source,last_used,name,workspaces_dir";

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
    })
}

pub const WORKSPACE_COLUMNS: &str =
    "id,repository_id,name,path,branch,state,error,base_commit,base_ref,git_dir,git_dir_id";

pub fn workspace(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: row.get("id")?,
        repository_id: row.get("repository_id")?,
        name: row.get("name")?,
        path: PathBuf::from(row.get::<_, String>("path")?),
        branch: row.get("branch")?,
        state: row.get("state")?,
        error: row.get("error")?,
        base_commit: row.get("base_commit")?,
        base_ref: row.get("base_ref")?,
        git_dir: row.get::<_, Option<String>>("git_dir")?.map(PathBuf::from),
        git_dir_id: row.get("git_dir_id")?,
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

const EXECUTION_COLUMNS: &str = "id,workspace_id,state,wrapper,child,group_id";

fn execution(row: &Row<'_>) -> rusqlite::Result<Execution> {
    Ok(Execution {
        id: row.get("id")?,
        workspace_id: row.get("workspace_id")?,
        state: row.get("state")?,
        wrapper: json_column(row, "wrapper")?,
        child: json_column(row, "child")?,
        group_id: row.get("group_id")?,
    })
}

fn executions_query() -> String {
    format!("SELECT {EXECUTION_COLUMNS} FROM executions WHERE workspace_id=?1")
}

pub fn executions(db: &Connection, workspace_id: &str) -> Result<Vec<Execution>> {
    Ok(db
        .prepare(&executions_query())?
        .query_map([workspace_id], execution)?
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
mod tests {
    use super::*;

    // Reconstruct old schemas from a frozen pre-refactor snapshot, independently
    // of MIGRATIONS, so missing or misnumbered steps cannot fix their own fixture.
    fn historical_database(version: i64) -> Result<Connection> {
        let db = Connection::open_in_memory()?;
        db.execute_batch("PRAGMA foreign_keys=ON;")?;
        if version == 0 {
            return Ok(db);
        }
        db.execute_batch(include_str!("../../tests/fixtures/schema_v17.sql"))?;
        if version >= 18 {
            db.execute_batch(
                "CREATE INDEX executions_workspace ON executions(workspace_id);
                CREATE INDEX workspaces_repository ON workspaces(repository_id);",
            )?;
        }
        if version >= 19 {
            db.execute_batch(
                "CREATE INDEX IF NOT EXISTS access_request_target
                ON access_requests(workspace_id,target_key);",
            )?;
        }
        if version < 4 {
            db.execute_batch("DROP INDEX repository_names;")?;
        }
        for (introduced, table, column) in [
            (16, "workspaces", "setup_finished"),
            (13, "repositories", "workspaces_dir"),
            (10, "executions", "group_id"),
            (10, "executions", "child"),
            (10, "executions", "wrapper"),
            (10, "workspaces", "git_dir_id"),
            (10, "workspaces", "git_dir"),
            (9, "resource_leases", "mode"),
            (5, "ports", "reason"),
            (4, "repositories", "name"),
            (2, "workspaces", "base_ref"),
            (2, "workspaces", "base_commit"),
        ] {
            if version < introduced {
                db.execute_batch(&format!("ALTER TABLE {table} DROP COLUMN {column};"))?;
            }
        }
        for (introduced, table) in [
            (17, "access_requests"),
            (15, "notifications"),
            (14, "pr_cleanup"),
            (12, "repository_configs"),
            (11, "repository_removals"),
            (8, "resource_leases"),
            (8, "resource_pools"),
            (7, "simulator_clean_requests"),
            (6, "simulators"),
            (3, "ports"),
        ] {
            if version < introduced {
                db.execute_batch(&format!("DROP TABLE {table};"))?;
            }
        }
        if version >= 19 {
            db.execute_batch(include_str!("store/simulator_owner.sql"))?;
        }
        db.pragma_update(None, "user_version", version)?;
        db.execute_batch(
            "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
            INSERT INTO workspaces(id,repository_id,name,path,branch,state)
                VALUES ('workspace','repo','worker','/work','worker','ready');
            INSERT INTO executions(id,workspace_id,state) VALUES ('execution','workspace','running');",
        )?;
        Ok(db)
    }

    fn schema_snapshot(db: &Connection) -> Result<Vec<String>> {
        Ok(db
            .prepare("SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")?
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|sql| Ok(sql?.split_whitespace().collect()))
            .collect::<rusqlite::Result<_>>()?)
    }

    #[test]
    fn migrates_every_recorded_version_to_the_same_schema() -> Result<()> {
        let expected = schema_snapshot(&historical_database(SCHEMA_VERSION)?)?;
        assert_eq!(MIGRATIONS.last().unwrap().0, SCHEMA_VERSION);
        for version in 0..=SCHEMA_VERSION {
            let mut db = historical_database(version)?;
            migrate(&mut db).with_context(|| format!("upgrade from version {version}"))?;
            assert_eq!(schema_snapshot(&db)?, expected, "from version {version}");
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
                SCHEMA_VERSION
            );
            if version > 0 {
                assert_eq!(
                    executions(&db, "workspace")?[0].state,
                    ExecutionState::Running
                );
                assert_eq!(
                    db.query_row("SELECT path FROM workspaces", [], |r| r.get::<_, String>(0))?,
                    "/work"
                );
            }
            migrate(&mut db)?;
            assert_eq!(schema_snapshot(&db)?, expected);
        }
        Ok(())
    }

    #[test]
    fn failed_migration_rolls_back_all_pending_steps() -> Result<()> {
        let mut db = historical_database(1)?;
        db.execute_batch("ALTER TABLE executions ADD COLUMN wrapper TEXT;")?;
        let before = schema_snapshot(&db)?;
        let error = migrate(&mut db).unwrap_err();
        assert!(format!("{error:#}").contains("apply schema migration 10"));
        assert_eq!(schema_snapshot(&db)?, before);
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
            1
        );
        db.execute_batch("ALTER TABLE executions DROP COLUMN wrapper;")?;
        migrate(&mut db)?;
        assert_eq!(
            executions(&db, "workspace")?[0].state,
            ExecutionState::Running
        );
        Ok(())
    }

    #[test]
    fn newer_schema_is_rejected_without_changes() -> Result<()> {
        let mut db = historical_database(17)?;
        db.pragma_update(None, "user_version", SCHEMA_VERSION + 1)?;
        let before = schema_snapshot(&db)?;
        assert!(
            migrate(&mut db)
                .unwrap_err()
                .to_string()
                .contains("newer Shoal version")
        );
        assert_eq!(schema_snapshot(&db)?, before);
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
            SCHEMA_VERSION + 1
        );
        Ok(())
    }

    #[test]
    fn exists_checks_bound_queries_in_the_current_transaction() -> Result<()> {
        let mut db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE records(name TEXT);")?;
        let tx = db.transaction()?;
        tx.execute("INSERT INTO records(name) VALUES (?1)", ["present"])?;
        let sql = "SELECT 1 FROM records WHERE name=?1";
        assert!(exists(&tx, sql, ["present"])?);
        assert!(!exists(&tx, sql, ["missing"])?);
        assert!(exists(&tx, "SELECT 1 FROM missing_table", []).is_err());
        tx.rollback()?;
        assert!(!exists(&db, sql, ["present"])?);
        Ok(())
    }

    #[test]
    fn mappers_use_named_columns() -> Result<()> {
        let mut db = Connection::open_in_memory()?;
        migrate(&mut db)?;
        db.execute_batch(
            r#"INSERT INTO repositories(id,path,source,last_used,name,workspaces_dir)
                VALUES ('repo','/checkout','origin',42,'project','/workspaces');
            INSERT INTO workspaces(id,repository_id,name,path,branch,state,error,
                base_commit,base_ref,git_dir,git_dir_id,setup_finished)
                VALUES ('workspace','repo','worker','/work','feature','failed','setup error',
                    'abc','refs/heads/main','/git/worktrees/worker','1:2',1);
            INSERT INTO ports(workspace_id,name,port,env_var,reason)
                VALUES ('workspace','web',12345,'WEB_PORT',NULL);
            INSERT INTO executions(id,workspace_id,state,wrapper,child,group_id)
                VALUES ('execution','workspace','unknown','{"pid":123,"birth":"wrapper"}',
                    '{"pid":456,"birth":"child"}',456);"#,
        )?;
        check_columns(
            &db,
            "repositories",
            REPOSITORY_COLUMNS,
            repository,
            serde_json::json!({
                "id": "repo", "path": "/checkout", "source": "origin", "last_used": 42,
                "name": "project", "workspaces_dir": "/workspaces"
            }),
        )?;
        check_columns(
            &db,
            "workspaces",
            WORKSPACE_COLUMNS,
            workspace,
            serde_json::json!({
                "id": "workspace", "repository_id": "repo", "name": "worker", "path": "/work",
                "branch": "feature", "state": "failed", "error": "setup error",
                "base_commit": "abc", "base_ref": "refs/heads/main",
                "git_dir": "/git/worktrees/worker", "git_dir_id": "1:2"
            }),
        )?;
        check_columns(
            &db,
            "ports",
            PORT_COLUMNS,
            port,
            serde_json::json!({
                "workspace_id": "workspace", "name": "web", "port": 12345,
                "env_var": "WEB_PORT", "reason": null
            }),
        )?;
        check_columns(
            &db,
            "executions",
            EXECUTION_COLUMNS,
            execution,
            serde_json::json!({
                "id": "execution", "workspace_id": "workspace", "state": "unknown",
                "wrapper": {"pid": 123, "birth": "wrapper"},
                "child": {"pid": 456, "birth": "child"}, "group_id": 456
            }),
        )?;
        assert!(setup_finished(&db, "workspace")?);
        Ok(())
    }

    fn check_columns<T: Serialize>(
        db: &Connection,
        table: &str,
        columns: &str,
        mapper: fn(&Row<'_>) -> rusqlite::Result<T>,
        expected: serde_json::Value,
    ) -> Result<()> {
        // Exercise the shared projection and a reordered projection with an
        // unrelated leading column, as a changed schema or query might supply.
        let reordered = format!(
            "'extra' AS unrelated,{}",
            columns.split(',').rev().collect::<Vec<_>>().join(",")
        );
        for projection in [columns, &reordered] {
            let record = db.query_row(&format!("SELECT {projection} FROM {table}"), [], mapper)?;
            assert_eq!(serde_json::to_value(record)?, expected);
        }
        Ok(())
    }

    #[test]
    fn execution_json_errors_report_the_named_column_index() -> Result<()> {
        let db = Connection::open_in_memory()?;
        let error = db
            .query_row(
                "SELECT 'bad json' AS child, NULL AS wrapper, 'e' AS id,
                'w' AS workspace_id, 'unknown' AS state, NULL AS group_id",
                [],
                execution,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, _)
        ));
        Ok(())
    }

    fn seed_interrupted_operations(db: &mut Connection) -> Result<()> {
        db.execute_batch(
            "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);",
        )?;
        for state in [
            "preparing",
            "removing",
            "stopping",
            "reconciling",
            "ready",
            "failed",
        ] {
            db.execute("INSERT INTO workspaces(id,repository_id,name,path,branch,state,error) VALUES (?1,'repo',?1,?1,?1,?1,'original error')", [state])?;
        }
        db.execute_batch("INSERT INTO executions(id,workspace_id,state) VALUES ('execution','preparing','running');
            INSERT INTO simulator_clean_requests(request_id,workspace_id,record)
                VALUES ('clean','preparing','{\"status\":\"requested\",\"reason\":\"keep audit\"}');")?;
        Ok(())
    }

    fn operation_snapshot(db: &mut Connection) -> Result<Vec<String>> {
        Ok(db
            .prepare(
                "SELECT json_array(id,state,error) FROM workspaces
            UNION ALL SELECT json_array(id,state) FROM executions
            UNION ALL SELECT record FROM simulator_clean_requests ORDER BY 1",
            )?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    #[tokio::test]
    async fn current_schema_reopen_preserves_active_operations() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = Store::open(path.clone()).await.unwrap();
        store.run(seed_interrupted_operations).await.unwrap();
        let before = store.run(operation_snapshot).await.unwrap();
        for _ in 0..2 {
            let reopened = Store::open(path.clone()).await.unwrap();
            assert_eq!(reopened.run(operation_snapshot).await.unwrap(), before);
        }
    }

    #[tokio::test]
    async fn failed_quarantine_rolls_back_without_undoing_migration() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = Store::open(path.clone()).await.unwrap();
        store.run(seed_interrupted_operations).await.unwrap();
        store
            .run(|db| {
                db.execute_batch(
                    "DROP TABLE access_requests;
                PRAGMA user_version=16;
                CREATE TRIGGER fail_quarantine BEFORE UPDATE OF state ON workspaces
                    BEGIN SELECT RAISE(ABORT, 'quarantine blocked'); END;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let before = store.run(operation_snapshot).await.unwrap();
        let migrated = Store::open(path.clone()).await.unwrap();
        let error = migrated
            .quarantine_interrupted_operations()
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("quarantine blocked"));
        assert_eq!(migrated.run(operation_snapshot).await.unwrap(), before);
        migrated.run(|db| {
            assert_eq!(db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?, SCHEMA_VERSION);
            db.execute("INSERT INTO access_requests VALUES ('request','preparing','pool','default','{}',1)", [])?;
            db.execute_batch("DROP TRIGGER fail_quarantine;")?;
            Ok(())
        }).await.unwrap();
        migrated.quarantine_interrupted_operations().await.unwrap();
        let recovered = migrated.run(operation_snapshot).await.unwrap();
        assert_ne!(recovered, before);
        let reopened = Store::open(path).await.unwrap();
        reopened.quarantine_interrupted_operations().await.unwrap();
        assert_eq!(reopened.run(operation_snapshot).await.unwrap(), recovered);
    }

    #[tokio::test]
    async fn approval_records_survive_reopen_and_follow_workspace_ownership() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = Store::open(path.clone()).await.unwrap();
        store.run(|db| {
            db.execute_batch("DROP TABLE access_requests;
                PRAGMA user_version=16;
                INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES ('workspace','repo','worker','/work','worker','ready');")?;
            Ok(())
        }).await.unwrap();
        let migrated = Store::open(path.clone()).await.unwrap();
        migrated.run(|db| {
            db.execute("INSERT INTO access_requests VALUES ('request','workspace','pool','default','{}',1)", [])?;
            assert!(db.execute("INSERT INTO access_requests VALUES ('duplicate','workspace','pool','default','{}',1)", []).is_err());
            assert!(db.execute("INSERT INTO access_requests VALUES ('orphan','missing','pool','default','{}',1)", []).is_err());
            Ok(())
        }).await.unwrap();
        Store::open(path)
            .await
            .unwrap()
            .run(|db| {
                let count = |db: &Connection| {
                    db.query_row("SELECT COUNT(*) FROM access_requests", [], |row| {
                        row.get::<_, u32>(0)
                    })
                };
                assert_eq!(count(db)?, 1);
                db.execute("UPDATE workspaces SET state='failed'", [])?;
                assert_eq!(count(db)?, 1);
                db.execute("DELETE FROM workspaces", [])?;
                assert_eq!(count(db)?, 0);
                Ok(())
            })
            .await
            .unwrap();
    }

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
                let repo = db.query_row(
                    &format!("SELECT {REPOSITORY_COLUMNS} FROM repositories"),
                    [],
                    repository,
                )?;
                assert_eq!(repo.id, "repo");
                assert!(repo.name.is_none() && repo.workspaces_dir.is_none());
                let workspace = db.query_row(
                    &format!("SELECT {WORKSPACE_COLUMNS} FROM workspaces"),
                    [],
                    workspace,
                )?;
                assert_eq!(workspace.name, "feature");
                assert!(workspace.base_commit.is_none() && workspace.base_ref.is_none());
                assert!(setup_finished(db, "workspace")?);
                assert_eq!(
                    executions(db, "workspace")?[0].state,
                    crate::state::ExecutionState::Running
                );
                assert!(ports(db, None)?.is_empty());
                Ok(())
            })
            .await
            .unwrap();
        store.quarantine_interrupted_operations().await.unwrap();
        Store::open(path)
            .await
            .unwrap()
            .run(|db| {
                assert_eq!(
                    executions(db, "workspace")?[0].state,
                    crate::state::ExecutionState::Unknown
                );
                Ok(())
            })
            .await
            .unwrap();
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
        migrated
            .run(|db| {
                let leases = crate::daemon::resources::leases(db, None)?;
                assert_eq!(leases.len(), 1);
                assert_eq!(leases[0].id, "lease");
                assert_eq!(leases[0].mode, crate::daemon::resources::LockMode::Permit);
                assert_eq!(leases[0].created_at, 123);
                let json: String =
                    db.query_row("SELECT definition FROM resource_pools", [], |row| {
                        row.get(0)
                    })?;
                let stored: crate::daemon::resources::Definition = serde_json::from_str(&json)?;
                let config: crate::config::repo::RepoConfig =
                    toml::from_str("[resources.worker]\ncapacity=2")?;
                assert_eq!(
                    stored,
                    crate::daemon::resources::definitions(
                        &config.resources,
                        &config.resource_pools
                    )?["worker"]
                );
                assert!(
                    db.execute("UPDATE resource_leases SET mode='invalid'", [])
                        .is_err()
                );
                assert!(
                    db.query_row("SELECT 'invalid'", [], |row| {
                        row.get::<_, crate::daemon::resources::LockMode>(0)
                    })
                    .is_err()
                );
                Ok(())
            })
            .await
            .unwrap();
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
                assert_eq!(state, WorkspaceState::Preparing);
                Ok(())
            })
            .await
            .unwrap();
        migrated.quarantine_interrupted_operations().await.unwrap();
        migrated
            .run(|db| {
                assert!(!setup_finished(db, "preparing")?);
                assert_eq!(
                    db.query_row(
                        "SELECT state FROM workspaces WHERE id='preparing'",
                        [],
                        |row| row.get::<_, WorkspaceState>(0)
                    )?,
                    WorkspaceState::Failed
                );
                Ok(())
            })
            .await
            .unwrap();
    }
}
