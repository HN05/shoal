//! SQLite persistence: schema migrations, row mappers, and the small
//! guards every mutation shares.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Params, Row};
use serde::{Serialize, de::DeserializeOwned};
use std::{path::PathBuf, time::Duration};

use crate::{
    model::{
        Execution, LinkedItem, PortReservation, Relation, Repository, Workspace, WorkspaceLinks,
    },
    sim::audit::CleanRequestStatus,
    state::{ExecutionState, WorkspaceState},
};

/// Schema version written by this build; older databases are migrated on open.
const SCHEMA_VERSION: i64 = 89;

mod attached_copies;
#[cfg(test)]
mod benchmark;
mod chat_names;
#[cfg(test)]
mod event_tests;
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

/// Runs before a migration's SQL to check existing rows, or to change them
/// where SQL cannot; failure aborts the upgrade.
type Prepare = fn(&Connection) -> Result<()>;

// Append schema changes in version order; startup recovery belongs in quarantine.
const MIGRATIONS: &[(i64, &str, Option<Prepare>)] = &[
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
        "CREATE INDEX IF NOT EXISTS access_request_target
            ON access_requests(workspace_id,target_key);",
        None,
    ),
    (
        20,
        include_str!("store/simulator_owner.sql"),
        Some(validate_simulator_records),
    ),
    (
        21,
        "CREATE TABLE IF NOT EXISTS workspace_completion (
            workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
            record TEXT NOT NULL
        );",
        None,
    ),
    (
        22,
        "CREATE TABLE IF NOT EXISTS workspace_issue (
            workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
            url TEXT NOT NULL, error TEXT
        );",
        None,
    ),
    (
        23,
        "CREATE TABLE IF NOT EXISTS repository_resource_leases (
            lease_id TEXT PRIMARY KEY REFERENCES resource_leases(id) ON DELETE CASCADE,
            repository_id TEXT NOT NULL REFERENCES repositories(id), path TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS repository_resource_owner ON repository_resource_leases(repository_id);",
        None,
    ),
    (
        24,
        "CREATE TABLE IF NOT EXISTS workspace_continuation (
            workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE
        );",
        None,
    ),
    (
        25,
        "CREATE TABLE IF NOT EXISTS pr_activity (
            workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            url TEXT NOT NULL, record TEXT NOT NULL,
            PRIMARY KEY(workspace_id,url)
        );",
        None,
    ),
    (
        26,
        "CREATE TABLE IF NOT EXISTS workspace_scopes (
            token TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE
        );",
        None,
    ),
    (
        27,
        "CREATE TABLE IF NOT EXISTS workspace_holds (
            workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            name TEXT NOT NULL, reason TEXT, created_at INTEGER NOT NULL,
            PRIMARY KEY(workspace_id,name)
        );",
        None,
    ),
    (
        28,
        include_str!("store/workspace_events.sql"),
        None,
    ),
    (
        29,
        "CREATE TABLE IF NOT EXISTS workspace_renames (
            workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
            name TEXT NOT NULL UNIQUE, branch TEXT NOT NULL
        );",
        None,
    ),
    (30, include_str!("store/workspace_review.sql"), None),
    (31, include_str!("store/workspace_undone.sql"), None),
    (
        32,
        "ALTER TABLE executions ADD COLUMN scope_token TEXT;",
        None,
    ),
    (33, include_str!("store/workspace_base.sql"), None),
    // Name the upgrade, not the retired command, as the migrated hold's reason.
    (
        34,
        "UPDATE workspace_holds SET reason='Converted from an earlier Shoal version'
            WHERE from_continuation AND reason='Kept by shoal continue';",
        None,
    ),
    (35, "ALTER TABLE workspace_issue ADD COLUMN title TEXT;", None),
    (36, include_str!("store/agent_messages.sql"), None),
    (37, include_str!("store/workspace_agent_state.sql"), None),
    (38, include_str!("store/agent_queue.sql"), None),
    (
        39,
        "ALTER TABLE repository_removals ADD COLUMN workspace_paths TEXT NOT NULL DEFAULT '[]';
         CREATE TABLE IF NOT EXISTS workspace_item_activity (
            workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            url TEXT NOT NULL,
            record TEXT NOT NULL,
            PRIMARY KEY(workspace_id,url)
        );",
        None,
    ),
    // `shoal access` became `shoal approvals`.
    (
        40,
        "UPDATE notifications SET kind='approval_requested',
            message=replace(message, 'shoal access approve', 'shoal approvals approve')
            WHERE kind='access_requested';",
        None,
    ),
    (41, include_str!("store/questions.sql"), None),
    (42, include_str!("store/chats.sql"), None),
    // `shoal approvals approve` became `shoal approve`.
    (
        43,
        "UPDATE notifications
            SET message=replace(message, 'shoal approvals approve', 'shoal approve')
            WHERE kind='approval_requested';",
        None,
    ),
    // Unix time of the last recorded activity, so idle periods survive restarts.
    (44, "ALTER TABLE workspaces ADD COLUMN active_at INTEGER;", None),
    (45, REMOVED_WORKSPACES, None),
    (46, include_str!("store/display_numbers.sql"), None),
    (47, include_str!("store/workspace_items.sql"), None),
    (48, include_str!("store/agent_queue_restores.sql"), None),
    (49, CLEANUP_GRACE, None),
    (50, RETIRED_KEEP_REQUESTS, None),
    (51, CHECKPOINTS, None),
    (52, REMOVAL_CHECKPOINTS, None),
    (53, KEPT_BRANCHES, None),
    // Unix time `done` closed an unanswered question.
    (54, "ALTER TABLE questions ADD COLUMN closed_at INTEGER;", None),
    (55, include_str!("store/workspace_parent.sql"), None),
    (56, QUEUE_TARGETS, None),
    (57, include_str!("store/chat_messages.sql"), None),
    (58, include_str!("store/repository_approvals.sql"), None),
    (59, QUESTION_WITHDRAWN, None),
    (60, NOTIFICATION_SENDERS, None),
    (61, CHAT_MESSAGE_SENDERS, None),
    (62, NOTIFICATION_SUBJECTS, None),
    (63, include_str!("store/agent_queue_dependencies.sql"), None),
    (64, FORGE_HOSTS, None),
    (65, REPOSITORY_AGENT_ENVIRONMENTS, None),
    (66, REPOSITORY_CONTRIBUTIONS, None),
    (67, REPOSITORY_MOVES, None),
    (68, "UPDATE notifications SET message=replace(message,
        'shoal approve ', 'shoal approvals --approve ')
        WHERE kind='approval_requested';
        UPDATE notifications SET message=replace(message,
        'shoal answer ', 'shoal questions ')
        WHERE kind='question_asked';", None),
    (69, DEMOS, None),
    (70, include_str!("store/workspace_turn_retries.sql"), None),
    (
        71,
        include_str!("store/chat_names.sql"),
        Some(chat_names::name_chats),
    ),
    (72, include_str!("store/agent_queue_stopped_restores.sql"), None),
    (73, include_str!("store/session_cleanup.sql"), None),
    (74, include_str!("store/execution_upstream_resets.sql"), None),
    (75, AUTOMATIC_RESTORES, None),
    (76, include_str!("store/places.sql"), None),
    (77, include_str!("store/owned_places.sql"), None),
    (78, EXECUTION_AGENTS, None),
    (79, include_str!("store/execution_unretried_turns.sql"), None),
    (80, "", Some(place_servers::key_by_server)),
    (81, PROMPT_CACHE_EXPIRY, None),
    (82, CHAT_RENAMES, None),
    (83, include_str!("store/demo_events.sql"), None),
    (84, include_str!("store/simulator_events.sql"), None),
    (85, AGENT_MESSAGE_ATTACHMENTS, None),
    (86, QUIET_AGENT_MESSAGES, None),
    (87, WORKSPACE_TITLES, None),
    (88, include_str!("store/workspace_files.sql"), None),
    (89, "", Some(attached_copies::record_existing)),
];

/// The JSON array of paths of the files attached to an agent message.
const AGENT_MESSAGE_ATTACHMENTS: &str =
    "ALTER TABLE agent_messages ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]';";
/// Messages that wait for an agent's next delivery without starting its
/// next turn.
const QUIET_AGENT_MESSAGES: &str = "ALTER TABLE agent_messages
    ADD COLUMN wakes INTEGER NOT NULL DEFAULT 1 CHECK(wakes IN (0,1));";
/// What a workspace's agent calls its work.
const WORKSPACE_TITLES: &str = "ALTER TABLE workspaces ADD COLUMN title TEXT;";

/// Journal a running chat's new name, which a chat still starting keeps.
const CHAT_RENAMES: &str = "ALTER TABLE chats ADD COLUMN starting INTEGER NOT NULL DEFAULT 0
    CHECK (starting IN (0,1));
CREATE TRIGGER chat_renamed AFTER UPDATE OF name ON chats
WHEN NEW.name IS NOT OLD.name BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_renamed','chat',json_object('id',NEW.id,'repository_id',NEW.repository_id,
            'name',NEW.name,'agent',NEW.agent,'pid',NEW.pid,'started_at',NEW.started_at)
    ));
END;";

/// The agent each execution runs, which keeps its reported session once the
/// execution ends without a stop of its own, as at a reboot.
const EXECUTION_AGENTS: &str = "ALTER TABLE executions ADD COLUMN agent TEXT;";

/// When the prompt cache of an agent idle since a turn ended expires.
const PROMPT_CACHE_EXPIRY: &str =
    "ALTER TABLE workspace_agent_state ADD COLUMN prompt_cache_expires_at INTEGER;";

/// Restores Shoal queued on its own, as after a signal stop, rather than
/// `shoal resume`: one of an agent stopped between turns waits for an update.
const AUTOMATIC_RESTORES: &str = "ALTER TABLE agent_queue
    ADD COLUMN automatic INTEGER NOT NULL DEFAULT 0 CHECK(automatic IN (0,1));";

/// The demo each running execution runs; a demo ends with its execution.
const DEMOS: &str = "CREATE TABLE demos (
    execution_id TEXT PRIMARY KEY REFERENCES executions(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    record TEXT NOT NULL,
    has_run INTEGER NOT NULL DEFAULT 0 CHECK(has_run IN (0,1)),
    UNIQUE(workspace_id,name)
);";

/// Repositories `shoal contribute` registered, which Shoal removes once
/// nothing of them is left, after a grace period like a workspace's.
const REPOSITORY_CONTRIBUTIONS: &str = "ALTER TABLE repositories
    ADD COLUMN contribution INTEGER NOT NULL DEFAULT 0 CHECK(contribution IN (0,1));
CREATE TABLE contribution_grace (
    repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
    cleanup_at INTEGER NOT NULL,
    claimed INTEGER NOT NULL CHECK(claimed IN (0, 1))
);";

/// A repository directory moving into a new root, with the identities that
/// prove which directory recovery may delete and the state each of its
/// workspaces returns to; `switched` once the records name the destination.
const REPOSITORY_MOVES: &str = "CREATE TABLE repository_moves (
    repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
    source TEXT NOT NULL,
    destination TEXT NOT NULL,
    source_id TEXT NOT NULL,
    destination_id TEXT NOT NULL,
    states TEXT NOT NULL,
    switched INTEGER NOT NULL DEFAULT 0 CHECK(switched IN (0, 1))
);";

/// The approval request a notification asks the user to decide, by its key,
/// so withdrawing the request finds it though the user edited its text.
const NOTIFICATION_SUBJECTS: &str = "ALTER TABLE notifications ADD COLUMN subject TEXT;";

/// Where the tracked agents of a repository's workspaces kept their
/// configuration, as JSON objects. Rows outlive the repository until the CLI
/// that removed it has cleaned their trust entries there.
const REPOSITORY_AGENT_ENVIRONMENTS: &str =
    "CREATE TABLE IF NOT EXISTS repository_agent_environments (
    repository_id TEXT NOT NULL,
    environment TEXT NOT NULL,
    PRIMARY KEY(repository_id, environment)
);";

/// The workspace a chat's message is about, by identity, which the user's
/// notification keeps when the chat ends first. Earlier messages name none.
const CHAT_MESSAGE_SENDERS: &str = "ALTER TABLE chat_messages ADD COLUMN workspace_id TEXT;";

/// The workspace a notification is about, by identity, so withdrawing what
/// it sent survives a rename and never reaches a later workspace of the same
/// name. Earlier notifications name none.
const NOTIFICATION_SENDERS: &str = "ALTER TABLE notifications ADD COLUMN workspace_id TEXT;";

/// Whether the asking agent withdrew a closed question.
const QUESTION_WITHDRAWN: &str = "ALTER TABLE questions ADD COLUMN withdrawn INTEGER NOT NULL DEFAULT 0 CHECK(withdrawn IN (0,1));";

/// The forge each host answered as when Shoal last asked it, and the
/// scheme it was asked over.
const FORGE_HOSTS: &str = "CREATE TABLE forge_hosts (
    host TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    scheme TEXT NOT NULL
);";

/// The existing workspace a queued launch opens, apart from the workspace a
/// starting launch reports it opened.
const QUEUE_TARGETS: &str =
    "ALTER TABLE agent_queue ADD COLUMN opens_workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE;
    CREATE INDEX IF NOT EXISTS agent_queue_opens ON agent_queue(opens_workspace_id);";

/// When the grace period of an unattended removal ends, and whether the
/// current sweep pass still wants to remove the workspace.
const CLEANUP_GRACE: &str = "CREATE TABLE IF NOT EXISTS cleanup_grace (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    cleanup_at INTEGER NOT NULL,
    claimed INTEGER NOT NULL CHECK(claimed IN (0, 1))
);";

/// Keep requests were retired: a completion still awaiting one requests the
/// cleanup the agent asked to skip, and every keep request goes.
const RETIRED_KEEP_REQUESTS: &str = "UPDATE workspace_completion
        SET record=json_set(record, '$.cleanup', json('true'))
        WHERE json_valid(record) AND json_extract(record, '$.keep_request') IS NOT NULL;
    UPDATE workspace_completion SET record=json_remove(record, '$.keep_request')
        WHERE json_valid(record);
    DELETE FROM access_requests WHERE target_key='keep';";

/// Checkpoints outlive their workspace, not their repository.
const CHECKPOINTS: &str = "CREATE TABLE checkpoints (
    workspace_id TEXT NOT NULL,
    number INTEGER NOT NULL,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL,
    record TEXT NOT NULL,
    PRIMARY KEY(workspace_id, number)
);
CREATE TABLE checkpoint_numbers (
    workspace_id TEXT PRIMARY KEY,
    last INTEGER NOT NULL
);";

/// A removal's snapshot becomes its workspace's first checkpoint, which the
/// removal record names; the daemon moves its ref on the next expiry pass.
const REMOVAL_CHECKPOINTS: &str =
    "INSERT INTO checkpoints(workspace_id,number,repository_id,expires_at,record)
        SELECT workspace_id, 1, repository_id, expires_at, json_object(
            'workspace_id', workspace_id,
            'repository_id', repository_id,
            'number', 1,
            'cause', 'removal',
            'branch', json_extract(record, '$.branch'),
            'head', json_extract(record, '$.head'),
            'commit', json_extract(record, '$.snapshot'),
            'files_omitted', json(CASE WHEN json_extract(record, '$.files_omitted')
                THEN 'true' ELSE 'false' END),
            'created_at', json_extract(record, '$.removed_at'),
            'expires_at', expires_at)
        FROM removed_workspaces WHERE json_valid(record);
    INSERT INTO checkpoint_numbers(workspace_id,last) SELECT workspace_id, 1 FROM checkpoints;
    UPDATE removed_workspaces SET record=json_remove(
            json_set(record, '$.checkpoint', (SELECT json(checkpoints.record) FROM checkpoints
                WHERE checkpoints.workspace_id=removed_workspaces.workspace_id)),
            '$.branch', '$.head', '$.snapshot', '$.files_omitted')
        WHERE json_valid(record);
    DELETE FROM removed_workspaces WHERE NOT json_valid(record);";

/// Branches removal kept because only they had some commits, at the commit
/// removal left them on; cleanup deletes them once those commits are kept
/// elsewhere. Existing removal records name the branches kept so far.
const KEPT_BRANCHES: &str = "CREATE TABLE kept_branches (
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    branch TEXT NOT NULL,
    head TEXT NOT NULL,
    kept_at INTEGER NOT NULL,
    PRIMARY KEY(repository_id, branch)
);
INSERT OR IGNORE INTO kept_branches(repository_id,branch,head,kept_at)
    SELECT repository_id, json_extract(record, '$.checkpoint.branch'),
        json_extract(record, '$.checkpoint.head'),
        COALESCE(json_extract(record, '$.removed_at'), 0)
    FROM removed_workspaces
    WHERE json_valid(record)
        AND json_extract(record, '$.checkpoint.branch') IS NOT NULL
        AND json_extract(record, '$.checkpoint.head') IS NOT NULL
    ORDER BY json_extract(record, '$.removed_at') DESC;";

/// Removal records outlive their workspace, not their repository.
const REMOVED_WORKSPACES: &str = "CREATE TABLE removed_workspaces (
    workspace_id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    record TEXT NOT NULL
);";

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
    for &(target, sql, prepare) in MIGRATIONS {
        if target > version {
            if let Some(prepare) = prepare {
                prepare(&tx)?;
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

/// The smallest positive number no row of `table` holds in its `number`
/// column, so the numbers users type stay short.
pub fn free_number(db: &Connection, table: &str) -> Result<i64> {
    Ok(db.query_row(
        &format!(
            "SELECT MIN(n) FROM (SELECT 1 AS n UNION ALL SELECT number+1 FROM {table})
                WHERE n NOT IN (SELECT number FROM {table})"
        ),
        [],
        |row| row.get(0),
    )?)
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

/// What holds `name`, other than the workspace `except_id`. Chats are
/// running as far as the caller ended finished ones first.
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

/// Refuse a name another workspace, an unfinished rename or a running chat holds.
pub fn ensure_name_available(db: &Connection, name: &str, except_id: Option<&str>) -> Result<()> {
    match name_holder(db, name, except_id)? {
        None => Ok(()),
        Some(NameHolder::Workspace) => anyhow::bail!("workspace name already exists: {name}"),
        Some(NameHolder::Rename) => {
            anyhow::bail!("workspace name is reserved by an unfinished rename: {name}")
        }
        Some(NameHolder::Chat) => anyhow::bail!("a running chat already has the name {name}"),
    }
}

/// Whether a workspace, an unfinished rename or a running chat holds `name`.
pub fn name_taken(db: &Connection, name: &str) -> Result<bool> {
    Ok(name_holder(db, name, None)?.is_some())
}

/// Check ownership before inserting a workspace or reserving a rename in a transaction.
/// Names (including pending renames) are global; branches belong to one repository.
pub fn ensure_name_and_branch_available(
    db: &Connection,
    repository_id: &str,
    name: &str,
    branch: &str,
    except_id: Option<&str>,
) -> Result<()> {
    ensure_name_available(db, name, except_id)?;
    let owner: Option<String> = db
        .query_row(
            "SELECT name FROM workspaces WHERE repository_id=?1 AND branch=?2
                AND (?3 IS NULL OR id!=?3)",
            rusqlite::params![repository_id, branch, except_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(owner) = owner {
        anyhow::bail!("branch is already owned by workspace {owner}");
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
        "repository move is incomplete; run shoal repo migrate again"
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
        "repository move is incomplete; run shoal repo migrate again"
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
    ensure!(ready, "workspace is not ready");
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

pub const WORKSPACE_COLUMNS: &str =
    "id,repository_id,name,title,path,branch,state,error,base_commit,base_ref,git_dir,git_dir_id";

pub fn workspace(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        holds: Vec::new(),
        review: Vec::new(),
        agent_state: None,
        base_workspace: None,
        stacked_workspaces: Vec::new(),
        parent_workspace: None,
        child_workspaces: Vec::new(),
        chat: None,
        links: WorkspaceLinks::default(),
        running: false,
        done: false,
        usage: None,
        id: row.get("id")?,
        repository_id: row.get("repository_id")?,
        name: row.get("name")?,
        title: row.get("title")?,
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

/// Read canonical links while keeping malformed legacy PR records out of list output.
pub fn workspace_links(db: &Connection, workspace_id: &str) -> rusqlite::Result<WorkspaceLinks> {
    let (issue, issue_title) = db
        .query_row(
            "SELECT url,title FROM workspace_issue WHERE workspace_id=?1",
            [workspace_id],
            |row| Ok((Some(row.get(0)?), row.get(1)?)),
        )
        .optional()?
        .unwrap_or_default();
    let prs = db
        .query_row(
            "SELECT record FROM pr_cleanup WHERE workspace_id=?1",
            [workspace_id],
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
    Ok(WorkspaceLinks {
        issue,
        issue_title,
        prs,
        opened: recorded_items(db, workspace_id, Relation::Opened)?,
        related: recorded_items(db, workspace_id, Relation::Related)?,
    })
}

/// The workspace's items with a recorded relation, oldest first.
pub fn recorded_items(
    db: &Connection,
    workspace_id: &str,
    relation: Relation,
) -> rusqlite::Result<Vec<LinkedItem>> {
    db.prepare(
        "SELECT kind,url,title FROM workspace_items WHERE workspace_id=?1 AND relation=?2
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

const EXECUTION_COLUMNS: &str = "id,workspace_id,state,wrapper,child,group_id,origin,agent";

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
mod tests;
