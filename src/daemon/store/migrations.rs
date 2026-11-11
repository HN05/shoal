//! Schema migrations: the version this build writes and the steps that
//! bring an older database to it, apart from the rest of the store, so that
//! two changes adding a migration both change this file.
use anyhow::{Context, Result, ensure};
use rusqlite::Connection;

use super::{chat_names, place_owners, place_servers};

/// Schema version written by this build; older databases are migrated on open.
pub(super) const SCHEMA_VERSION: i64 = 148;

/// Runs before a migration's SQL to check existing rows, or to change them
/// where SQL cannot; failure aborts the upgrade.
type Prepare = fn(&Connection) -> Result<()>;

// Append schema changes in version order; startup recovery belongs in quarantine.
pub(super) const MIGRATIONS: &[(i64, &str, Option<Prepare>)] = &[
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
        include_str!("simulator_owner.sql"),
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
        include_str!("workspace_events.sql"),
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
    (30, include_str!("workspace_review.sql"), None),
    (31, include_str!("workspace_undone.sql"), None),
    (
        32,
        "ALTER TABLE executions ADD COLUMN scope_token TEXT;",
        None,
    ),
    (33, include_str!("workspace_base.sql"), None),
    // Name the upgrade, not the retired command, as the migrated hold's reason.
    (
        34,
        "UPDATE workspace_holds SET reason='Converted from an earlier Shoal version'
            WHERE from_continuation AND reason='Kept by shoal continue';",
        None,
    ),
    (35, "ALTER TABLE workspace_issue ADD COLUMN title TEXT;", None),
    (36, include_str!("agent_messages.sql"), None),
    (37, include_str!("workspace_agent_state.sql"), None),
    (38, include_str!("agent_queue.sql"), None),
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
    (41, include_str!("questions.sql"), None),
    (42, include_str!("chats.sql"), None),
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
    (46, include_str!("display_numbers.sql"), None),
    (47, include_str!("workspace_items.sql"), None),
    (48, include_str!("agent_queue_restores.sql"), None),
    (49, CLEANUP_GRACE, None),
    (50, RETIRED_KEEP_REQUESTS, None),
    (51, CHECKPOINTS, None),
    (52, REMOVAL_CHECKPOINTS, None),
    (53, KEPT_BRANCHES, None),
    // Unix time `done` closed an unanswered question.
    (54, "ALTER TABLE questions ADD COLUMN closed_at INTEGER;", None),
    (55, include_str!("workspace_parent.sql"), None),
    (56, QUEUE_TARGETS, None),
    (57, include_str!("chat_messages.sql"), None),
    (58, include_str!("repository_approvals.sql"), None),
    (59, QUESTION_WITHDRAWN, None),
    (60, NOTIFICATION_SENDERS, None),
    (61, CHAT_MESSAGE_SENDERS, None),
    (62, NOTIFICATION_SUBJECTS, None),
    (63, include_str!("agent_queue_dependencies.sql"), None),
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
    (70, include_str!("workspace_turn_retries.sql"), None),
    (
        71,
        include_str!("chat_names.sql"),
        Some(chat_names::name_chats),
    ),
    (72, include_str!("agent_queue_stopped_restores.sql"), None),
    (73, include_str!("session_cleanup.sql"), None),
    (74, include_str!("execution_upstream_resets.sql"), None),
    (75, AUTOMATIC_RESTORES, None),
    (76, include_str!("places.sql"), None),
    (77, include_str!("owned_places.sql"), None),
    (78, EXECUTION_AGENTS, None),
    (79, include_str!("execution_unretried_turns.sql"), None),
    (80, "", Some(place_servers::key_by_server)),
    (81, PROMPT_CACHE_EXPIRY, None),
    (82, CHAT_RENAMES, None),
    (83, include_str!("demo_events.sql"), None),
    (84, include_str!("simulator_events.sql"), None),
    (85, AGENT_MESSAGE_ATTACHMENTS, None),
    (86, QUIET_AGENT_MESSAGES, None),
    (87, WORKSPACE_TITLES, None),
    (88, include_str!("workspace_files.sql"), None),
    (89, EARLIER_ATTACHED_FILES, None),
    (90, include_str!("workspace_item_runs.sql"), None),
    (91, PARENT_REMOVAL, None),
    (92, HOLD_PLACERS, None),
    // The local repository config moved to a file in the repository's directory.
    (93, "DROP TABLE IF EXISTS repository_configs;", None),
    (94, FILE_ATTACHERS, None),
    (95, EXECUTION_AGENT_PARENTS, None),
    (96, FILE_TITLES, None),
    (97, CHAT_QUESTIONS, None),
    (98, FILE_SPACES, None),
    (99, ENDED_CHATS, None),
    (100, EXECUTION_MODELS, None),
    (101, CHAT_SESSIONS, None),
    (102, include_str!("reviews.sql"), None),
    (103, include_str!("file_events.sql"), None),
    (104, include_str!("saved_chats.sql"), None),
    (105, STOPPED_CHAT_PROCESSES, None),
    (106, include_str!("review_false_positive_failures.sql"), None),
    (107, CHAT_MESSAGE_ATTACHMENTS, None),
    (108, include_str!("question_spaces.sql"), None),
    (109, include_str!("link_spaces.sql"), None),
    (110, include_str!("review_spaces.sql"), None),
    (111, CHAT_COMPLETION, None),
    (112, include_str!("pending_updates.sql"), None),
    (113, REVIEW_REPLY_LOOKUPS, None),
    (114, SENT_FILES, None),
    (115, include_str!("changed_on_both_sides.sql"), None),
    (116, include_str!("space_cleanup.sql"), None),
    (117, WORKSPACE_COMMITS, Some(allow_branchless_workspaces)),
    (118, SKIPPED_SETUP, None),
    (119, DISPOSABLE_WORKSPACES, None),
    (120, MERGE_REQUESTS, None),
    (121, include_str!("space_parents.sql"), None),
    (122, include_str!("chat_scopes.sql"), None),
    (123, REQUEST_SPACES, None),
    (124, CHAT_WORKSPACES, None),
    (125, include_str!("runners.sql"), None),
    (126, include_str!("agent_queue_jobs.sql"), None),
    (127, include_str!("ci_jobs.sql"), None),
    (128, include_str!("space_numbers.sql"), None),
    (129, "", Some(super::item_numbers::number_items)),
    (130, REPOSITORY_RUNNERS_OPTIONAL, None),
    (131, CI_JOB_RUNNERS, None),
    (132, include_str!("merges.sql"), None),
    (133, SPACE_MODES, None),
    (134, include_str!("space_agent_state.sql"), None),
    (135, WORKSPACE_COPIES, None),
    (136, "", Some(allow_chats_without_repository)),
    (137, "", Some(allow_files_without_repository)),
    (138, CHAT_STOPPERS, None),
    (139, "", Some(super::space_names::rename_invalid_names)),
    (140, include_str!("space_events.sql"), None),
    (141, AUTOMATIC_WRAPPER_RESTORES, None),
    (142, include_str!("space_progress.sql"), None),
    (143, PERMISSION_NARROWINGS, None),
    (144, include_str!("spaces.sql"), None),
    (145, "", Some(super::space_names::rename_invalid_copies)),
    (146, CI_JOB_PEAK_CPU_DROPPED, None),
    (147, "", Some(place_owners::own_by_space)),
    (148, include_str!("space_conversions.sql"), None),
];

/// Admission counts a CI job at its memory alone, so its runs' peak CPU is
/// no longer kept. Nothing references either table, so each is rebuilt,
/// which a migration from an earlier version applies again.
pub(super) const CI_JOB_PEAK_CPU_DROPPED: &str = "CREATE TABLE ci_jobs_memory (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL UNIQUE REFERENCES workspaces(id) ON DELETE CASCADE,
    execution_id TEXT,
    job TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    launched_at INTEGER,
    peak_memory INTEGER,
    usage_recorded INTEGER NOT NULL DEFAULT 0 CHECK(usage_recorded IN (0,1)),
    runner_id INTEGER
);
INSERT INTO ci_jobs_memory(id,repository_id,workspace_id,execution_id,job,started_at,launched_at,peak_memory,usage_recorded,runner_id)
    SELECT id,repository_id,workspace_id,execution_id,job,started_at,launched_at,peak_memory,usage_recorded,runner_id FROM ci_jobs;
DROP TABLE ci_jobs;
ALTER TABLE ci_jobs_memory RENAME TO ci_jobs;
CREATE TABLE ci_job_usage_memory (
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    workflow TEXT NOT NULL,
    name TEXT NOT NULL,
    ended_at INTEGER NOT NULL,
    peak_memory INTEGER NOT NULL
);
INSERT INTO ci_job_usage_memory(repository_id,workflow,name,ended_at,peak_memory)
    SELECT repository_id,workflow,name,ended_at,peak_memory FROM ci_job_usage;
DROP TABLE ci_job_usage;
ALTER TABLE ci_job_usage_memory RENAME TO ci_job_usage;
CREATE INDEX ci_job_usage_jobs ON ci_job_usage(repository_id, workflow, name, ended_at);";

/// The space whose process stopped a stopped chat, such as by a restart
/// whose restore failed, which may resume it; NULL for a chat the user or
/// Shoal stopped, or whose agent exited, which is the user's to resume.
pub(super) const CHAT_STOPPERS: &str = "ALTER TABLE chats ADD COLUMN stopped_by TEXT;";

/// The workspace a copy was made from, its checkpoint the copy started
/// from, the number of the copy's own checkpoint of what it started with,
/// which keeps that checkpoint from expiring, and the tree of the index it
/// started with, which no checkpoint records.
pub(super) const WORKSPACE_COPIES: &str = "ALTER TABLE workspaces ADD COLUMN copied_from TEXT;
ALTER TABLE workspaces ADD COLUMN copied_from_checkpoint INTEGER;
ALTER TABLE workspaces ADD COLUMN starting_checkpoint INTEGER;
ALTER TABLE workspaces ADD COLUMN starting_index TEXT;";

/// The runner the forge registered for a claimed job alone, deleted once
/// the job ends. Nothing references the table, so it is rebuilt, which a
/// migration from an earlier version applies again; the peak CPU a later
/// version drops is left behind.
pub(super) const CI_JOB_RUNNERS: &str = "CREATE TABLE ci_jobs_runners (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL UNIQUE REFERENCES workspaces(id) ON DELETE CASCADE,
    execution_id TEXT,
    job TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    launched_at INTEGER,
    peak_cpu REAL,
    peak_memory INTEGER,
    usage_recorded INTEGER NOT NULL DEFAULT 0 CHECK(usage_recorded IN (0,1)),
    runner_id INTEGER
);
INSERT INTO ci_jobs_runners(id,repository_id,workspace_id,execution_id,job,started_at,launched_at,peak_memory,usage_recorded)
    SELECT id,repository_id,workspace_id,execution_id,job,started_at,launched_at,peak_memory,usage_recorded FROM ci_jobs;
DROP TABLE ci_jobs;
ALTER TABLE ci_jobs_runners RENAME TO ci_jobs;";

/// A repository on a forge whose runners each take one job keeps no runner
/// of its own: its row records only the forge repository it serves. Nothing
/// references the table, so it is rebuilt.
pub(super) const REPOSITORY_RUNNERS_OPTIONAL: &str = "CREATE TABLE runners_optional (
    repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
    forge_repository TEXT NOT NULL,
    forge_id INTEGER,
    uuid TEXT,
    token TEXT,
    CHECK((forge_id IS NULL) = (uuid IS NULL) AND (uuid IS NULL) = (token IS NULL))
);
INSERT INTO runners_optional SELECT repository_id,forge_repository,forge_id,uuid,token FROM runners;
DROP TABLE runners;
ALTER TABLE runners_optional RENAME TO runners;";

/// The mode each agent started in, which its space, execution and queue
/// entry record. Every agent before modes ran a workspace's worker or a
/// chat.
pub(super) const SPACE_MODES: &str = "ALTER TABLE workspaces ADD COLUMN mode TEXT;
ALTER TABLE executions ADD COLUMN mode TEXT;
ALTER TABLE agent_queue ADD COLUMN mode TEXT;
ALTER TABLE chats ADD COLUMN mode TEXT NOT NULL DEFAULT 'chat';
UPDATE executions SET mode='worker' WHERE agent IS NOT NULL;
UPDATE workspaces SET mode='worker'
WHERE id IN (SELECT workspace_id FROM executions WHERE agent IS NOT NULL);";

/// The `--permission` narrowing each agent started with, beside its mode: a
/// JSON object of the keys it narrows, or none.
pub(super) const PERMISSION_NARROWINGS: &str = "ALTER TABLE workspaces ADD COLUMN narrowing TEXT;
ALTER TABLE executions ADD COLUMN narrowing TEXT;
ALTER TABLE agent_queue ADD COLUMN narrowing TEXT;
ALTER TABLE chats ADD COLUMN narrowing TEXT;";

/// The workspace whose worktree a chat runs in instead of its repository's
/// checkout. Removing the workspace removes its chats first, so nothing
/// cascades.
pub(super) const CHAT_WORKSPACES: &str =
    "ALTER TABLE chats ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);
    CREATE INDEX chats_workspace ON chats(workspace_id);";

/// Whether every removal deletes a workspace whatever it holds.
pub(super) const DISPOSABLE_WORKSPACES: &str = "ALTER TABLE workspaces ADD COLUMN disposable INTEGER NOT NULL DEFAULT 0 CHECK(disposable IN (0,1));";

/// Whether a workspace was created without its setup, which also skips its
/// removal hooks until `shoal setup` runs it.
pub(super) const SKIPPED_SETUP: &str = "ALTER TABLE workspaces ADD COLUMN setup_skipped INTEGER NOT NULL DEFAULT 0 CHECK(setup_skipped IN (0,1));";

/// Where a workspace without a branch started, and the tag, branch or PR it
/// was created for as JSON.
pub(super) const WORKSPACE_COMMITS: &str = "ALTER TABLE workspaces ADD COLUMN start_commit TEXT;
ALTER TABLE workspaces ADD COLUMN made_for TEXT;";

/// Drop the `NOT NULL` constraint on `workspaces.branch`.
pub(super) fn allow_branchless_workspaces(db: &Connection) -> Result<()> {
    drop_not_null(db, "workspaces", "branch TEXT")
}

/// Drop the `NOT NULL` constraint on `chats.repository_id`: a chat that
/// manages every repository's spaces has none of its own. Such a chat
/// records the `root_dir` it started in, which keeps its temporary
/// directory as a repository's directory keeps its chats'.
pub(super) fn allow_chats_without_repository(db: &Connection) -> Result<()> {
    drop_not_null(db, "chats", "repository_id TEXT")?;
    db.execute_batch("ALTER TABLE chats ADD COLUMN root_dir TEXT")?;
    Ok(())
}

/// Drop the `NOT NULL` constraint on `workspace_files.repository_id`: a
/// chat without a repository keeps its files in its `root_dir`, which each
/// of their records names.
pub(super) fn allow_files_without_repository(db: &Connection) -> Result<()> {
    drop_not_null(db, "workspace_files", "repository_id TEXT")?;
    db.execute_batch("ALTER TABLE workspace_files ADD COLUMN root_dir TEXT")?;
    Ok(())
}

/// Drop the `NOT NULL` constraint of the column `table` defines as
/// `column`, its name and type, in place, as SQLite documents for removing
/// a constraint: rebuilding the table would delete the rows of every table
/// that references it.
fn drop_not_null(db: &Connection, table: &str, column: &str) -> Result<()> {
    let sql: String = db.query_row(
        "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?1",
        [table],
        |row| row.get(0),
    )?;
    let constrained = format!("{column} NOT NULL");
    match sql.matches(&constrained).count() {
        0 => return Ok(()),
        1 => {}
        _ => anyhow::bail!("unexpected {table} table definition"),
    }
    let version: i64 = db.query_row("PRAGMA schema_version", [], |row| row.get(0))?;
    db.pragma_update(None, "writable_schema", true)?;
    db.execute(
        "UPDATE sqlite_schema SET sql=?1 WHERE type='table' AND name=?2",
        [sql.replacen(&constrained, column, 1), table.to_owned()],
    )?;
    db.pragma_update(None, "schema_version", version + 1)?;
    db.pragma_update(None, "writable_schema", false)?;
    Ok(())
}

/// The JSON array of the record IDs of the files an agent sent the user with
/// an alert or a question.
pub(super) const SENT_FILES: &str =
    "ALTER TABLE notifications ADD COLUMN files TEXT NOT NULL DEFAULT '[]';
    ALTER TABLE questions ADD COLUMN files TEXT NOT NULL DEFAULT '[]';";

/// `shoal land` became `shoal merge`, so its requests become merge requests.
const MERGE_REQUESTS: &str = "UPDATE access_requests SET target_key='merge',
        record=CASE WHEN json_valid(record)
            THEN json_set(record, '$.target', 'merge') ELSE record END
    WHERE target_key='land';";

/// An approval request names the space that made it, a workspace or a chat.
const REQUEST_SPACES: &str = "UPDATE access_requests SET record=json_set(
        json_remove(record, '$.workspace_id', '$.workspace'),
        '$.space_id', json_extract(record, '$.workspace_id'),
        '$.space', COALESCE(json_extract(record, '$.workspace'), ''))
    WHERE json_valid(record) AND json_type(record, '$.workspace_id') IS NOT NULL;";

/// The replies to a review's findings, which every read of the review loads.
pub(super) const REVIEW_REPLY_LOOKUPS: &str =
    "CREATE INDEX IF NOT EXISTS review_replies_review ON review_replies(review_id, id);";

/// The JSON completion a chat recorded with `shoal done`, until `--undo`.
pub(super) const CHAT_COMPLETION: &str = "ALTER TABLE chats ADD COLUMN completion TEXT;";

/// The JSON array of paths of the files attached to a chat's message.
pub(super) const CHAT_MESSAGE_ATTACHMENTS: &str =
    "ALTER TABLE chat_messages ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]';";

/// Whether an agent asked the question in its chat, where the user may
/// reply instead of answering it, and whether the agent an execution runs
/// marked its current turn as asking there, which a daemon restart keeps.
pub(super) const CHAT_QUESTIONS: &str =
    "ALTER TABLE questions ADD COLUMN in_chat INTEGER NOT NULL DEFAULT 0 CHECK(in_chat IN (0,1));
    ALTER TABLE executions ADD COLUMN asks_in_chat INTEGER NOT NULL DEFAULT 0 CHECK(asks_in_chat IN (0,1));";

/// The agent execution whose process tree registered each execution, which
/// a file it attaches names.
pub(super) const EXECUTION_AGENT_PARENTS: &str =
    "ALTER TABLE executions ADD COLUMN agent_execution TEXT;";

/// Who attached each file, as JSON; files attached before Shoal recorded it
/// have none.
pub(super) const FILE_ATTACHERS: &str = "ALTER TABLE workspace_files ADD COLUMN attached_by TEXT;";

/// The repositories whose copies of files attached to messages before Shoal
/// recorded them the cleanup sweep has yet to record, once no move of their
/// directory is unfinished.
pub(super) const EARLIER_ATTACHED_FILES: &str =
    "CREATE TABLE IF NOT EXISTS earlier_attached_files (repository_id TEXT PRIMARY KEY);
    INSERT OR IGNORE INTO earlier_attached_files SELECT id FROM repositories;";

/// What an attached file is, as whoever attached it said.
pub(super) const FILE_TITLES: &str = "ALTER TABLE workspace_files ADD COLUMN title TEXT;";

/// The workspace or chat each file is attached to, whose IDs never collide,
/// and its name then.
pub(super) const FILE_SPACES: &str =
    "ALTER TABLE workspace_files RENAME COLUMN workspace_id TO space_id;
    ALTER TABLE workspace_files RENAME COLUMN workspace TO space;";

/// The removed chats whose attached files stay until `expires_at`.
pub(super) const ENDED_CHATS: &str = "CREATE TABLE ended_chats (
    chat_id TEXT PRIMARY KEY,
    expires_at INTEGER NOT NULL
);";

/// The processes a stop of each chat was about to signal, with the
/// never-reused ID, as JSON, where the platform reports one, so a later stop
/// and the check before a restore or discard follow what they started once
/// an interrupted stop forgot them.
pub(super) const STOPPED_CHAT_PROCESSES: &str =
    "CREATE TABLE IF NOT EXISTS chat_stopped_processes (
    chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    pid INTEGER NOT NULL,
    birth TEXT NOT NULL,
    unique_id TEXT,
    PRIMARY KEY(chat_id, pid, birth)
);";

/// The JSON array of paths of the files attached to an agent message.
pub(super) const AGENT_MESSAGE_ATTACHMENTS: &str =
    "ALTER TABLE agent_messages ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]';";
/// Messages that wait for an agent's next delivery without starting its
/// next turn.
pub(super) const QUIET_AGENT_MESSAGES: &str = "ALTER TABLE agent_messages
    ADD COLUMN wakes INTEGER NOT NULL DEFAULT 1 CHECK(wakes IN (0,1));";
/// Removal takes a workspace's children before it, so none moves up. Tests
/// emulating an earlier database may have dropped the trigger already.
pub(super) const PARENT_REMOVAL: &str = "DROP TRIGGER IF EXISTS workspace_parent_removed;";
/// Who placed each hold; earlier holds were the user's and host apps'.
pub(super) const HOLD_PLACERS: &str = "ALTER TABLE workspace_holds
    ADD COLUMN placed_by TEXT NOT NULL DEFAULT 'user' CHECK(placed_by IN ('user','agent'));";
/// What a workspace's agent calls its work.
pub(super) const WORKSPACE_TITLES: &str = "ALTER TABLE workspaces ADD COLUMN title TEXT;";

/// Journal a running chat's new name, which a chat still starting keeps.
pub(super) const CHAT_RENAMES: &str =
    "ALTER TABLE chats ADD COLUMN starting INTEGER NOT NULL DEFAULT 0
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
pub(super) const EXECUTION_AGENTS: &str = "ALTER TABLE executions ADD COLUMN agent TEXT;";

/// The agent session each chat's hooks reported last, which an export of
/// the chat continues on another machine.
pub(super) const CHAT_SESSIONS: &str = "ALTER TABLE chats ADD COLUMN session TEXT;";

/// The model and effort each execution's agent runs with.
pub(super) const EXECUTION_MODELS: &str = "ALTER TABLE executions ADD COLUMN model TEXT;
    ALTER TABLE executions ADD COLUMN effort TEXT;";

/// When the prompt cache of an agent idle since a turn ended expires.
pub(super) const PROMPT_CACHE_EXPIRY: &str =
    "ALTER TABLE workspace_agent_state ADD COLUMN prompt_cache_expires_at INTEGER;";

/// Restores Shoal queued on its own, as after a signal stop, rather than
/// `shoal resume`: one of an agent stopped between turns waits for an update.
pub(super) const AUTOMATIC_RESTORES: &str = "ALTER TABLE agent_queue
    ADD COLUMN automatic INTEGER NOT NULL DEFAULT 0 CHECK(automatic IN (0,1));";

/// The restores waiting wrappers carry out are Shoal's own too, until
/// `shoal resume` asks for one.
pub(super) const AUTOMATIC_WRAPPER_RESTORES: &str =
    "UPDATE agent_queue SET automatic=1 WHERE kind='restore' AND env IS NULL;";

/// The demo each running execution runs; a demo ends with its execution.
pub(super) const DEMOS: &str = "CREATE TABLE demos (
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
pub(super) const REPOSITORY_CONTRIBUTIONS: &str = "ALTER TABLE repositories
    ADD COLUMN contribution INTEGER NOT NULL DEFAULT 0 CHECK(contribution IN (0,1));
CREATE TABLE contribution_grace (
    repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
    cleanup_at INTEGER NOT NULL,
    claimed INTEGER NOT NULL CHECK(claimed IN (0, 1))
);";

/// A repository directory moving into a new root, with the identities that
/// prove which directory recovery may delete and the state each of its
/// workspaces returns to; `switched` once the records name the destination.
pub(super) const REPOSITORY_MOVES: &str = "CREATE TABLE repository_moves (
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
pub(super) const NOTIFICATION_SUBJECTS: &str = "ALTER TABLE notifications ADD COLUMN subject TEXT;";

/// Where the tracked agents of a repository's workspaces kept their
/// configuration, as JSON objects. Rows outlive the repository until the CLI
/// that removed it has cleaned their trust entries there.
pub(super) const REPOSITORY_AGENT_ENVIRONMENTS: &str =
    "CREATE TABLE IF NOT EXISTS repository_agent_environments (
    repository_id TEXT NOT NULL,
    environment TEXT NOT NULL,
    PRIMARY KEY(repository_id, environment)
);";

/// The workspace a chat's message is about, by identity, which the user's
/// notification keeps when the chat is removed first. Earlier messages name none.
pub(super) const CHAT_MESSAGE_SENDERS: &str =
    "ALTER TABLE chat_messages ADD COLUMN workspace_id TEXT;";

/// The workspace a notification is about, by identity, so withdrawing what
/// it sent survives a rename and never reaches a later workspace of the same
/// name. Earlier notifications name none.
pub(super) const NOTIFICATION_SENDERS: &str =
    "ALTER TABLE notifications ADD COLUMN workspace_id TEXT;";

/// Whether the asking agent withdrew a closed question.
pub(super) const QUESTION_WITHDRAWN: &str = "ALTER TABLE questions ADD COLUMN withdrawn INTEGER NOT NULL DEFAULT 0 CHECK(withdrawn IN (0,1));";

/// The forge each host answered as when Shoal last asked it, and the
/// scheme it was asked over.
pub(super) const FORGE_HOSTS: &str = "CREATE TABLE forge_hosts (
    host TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    scheme TEXT NOT NULL
);";

/// The existing workspace a queued launch opens, apart from the workspace a
/// starting launch reports it opened.
pub(super) const QUEUE_TARGETS: &str =
    "ALTER TABLE agent_queue ADD COLUMN opens_workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE;
    CREATE INDEX IF NOT EXISTS agent_queue_opens ON agent_queue(opens_workspace_id);";

/// When the grace period of an unattended removal ends, and whether the
/// current sweep pass still wants to remove the workspace.
pub(super) const CLEANUP_GRACE: &str = "CREATE TABLE IF NOT EXISTS cleanup_grace (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    cleanup_at INTEGER NOT NULL,
    claimed INTEGER NOT NULL CHECK(claimed IN (0, 1))
);";

/// Keep requests were retired: a completion still awaiting one requests the
/// cleanup the agent asked to skip, and every keep request goes.
pub(super) const RETIRED_KEEP_REQUESTS: &str = "UPDATE workspace_completion
        SET record=json_set(record, '$.cleanup', json('true'))
        WHERE json_valid(record) AND json_extract(record, '$.keep_request') IS NOT NULL;
    UPDATE workspace_completion SET record=json_remove(record, '$.keep_request')
        WHERE json_valid(record);
    DELETE FROM access_requests WHERE target_key='keep';";

/// Checkpoints outlive their workspace, not their repository.
pub(super) const CHECKPOINTS: &str = "CREATE TABLE checkpoints (
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
pub(super) const REMOVAL_CHECKPOINTS: &str =
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
pub(super) const KEPT_BRANCHES: &str = "CREATE TABLE kept_branches (
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
pub(super) const REMOVED_WORKSPACES: &str = "CREATE TABLE removed_workspaces (
    workspace_id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    record TEXT NOT NULL
);";

pub(super) fn migrate(db: &mut Connection) -> Result<()> {
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
    crate::daemon::sim::records::list(db, None)
        .context("validate simulator records before indexing ownership")?;
    Ok(())
}
