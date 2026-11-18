-- The schema the migrations in migrations.rs make, which a new database
-- takes. Written by
-- SHOAL_WRITE_SCHEMA=1 cargo nextest run -p shoal-daemon -E 'test(a_new_database_takes)'

CREATE TABLE repositories (
            id TEXT PRIMARY KEY, path TEXT NOT NULL UNIQUE, source TEXT NOT NULL, last_used INTEGER NOT NULL
        , name TEXT, workspaces_dir TEXT, contribution INTEGER NOT NULL DEFAULT 0 CHECK(contribution IN (0,1)));

CREATE TABLE executions (
            id TEXT PRIMARY KEY, space_id TEXT NOT NULL, state TEXT NOT NULL
        , wrapper TEXT, child TEXT, group_id INTEGER, scope_token TEXT, origin TEXT, agent TEXT, agent_execution TEXT, asks_in_chat INTEGER NOT NULL DEFAULT 0 CHECK(asks_in_chat IN (0,1)), model TEXT, effort TEXT, mode TEXT, narrowing TEXT);

CREATE TABLE ports (
            workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            name TEXT NOT NULL, port INTEGER NOT NULL UNIQUE CHECK(port BETWEEN 1 AND 65535),
            env_var TEXT NOT NULL, reason TEXT, PRIMARY KEY(workspace_id, name), UNIQUE(workspace_id, env_var)
        );

CREATE TABLE simulators(id TEXT PRIMARY KEY, record TEXT NOT NULL);

CREATE TABLE simulator_clean_requests (
            id INTEGER PRIMARY KEY AUTOINCREMENT, request_id TEXT NOT NULL UNIQUE,
            workspace_id TEXT NOT NULL, record TEXT NOT NULL
        );

CREATE TABLE resource_pools (
            scope TEXT NOT NULL, name TEXT NOT NULL, definition TEXT NOT NULL, PRIMARY KEY(scope,name)
        );

CREATE TABLE resource_leases (
            id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            scope TEXT NOT NULL, pool TEXT NOT NULL, name TEXT NOT NULL, resource TEXT NOT NULL,
            reason TEXT, created_at INTEGER NOT NULL, mode TEXT NOT NULL DEFAULT 'permit' CHECK(mode IN ('permit','read','write')), acquirer TEXT NOT NULL DEFAULT 'user'
    CHECK(acquirer IN ('user','agent','demo')), acquirer_execution TEXT
    CHECK((acquirer='user') = (acquirer_execution IS NULL)), UNIQUE(workspace_id,pool,name),
            FOREIGN KEY(scope,pool) REFERENCES resource_pools(scope,name)
        );

CREATE TABLE repository_removals (
            repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
            directory_id TEXT,
            deleting_files INTEGER NOT NULL DEFAULT 0 CHECK(deleting_files IN (0,1))
        , workspace_paths TEXT NOT NULL DEFAULT '[]');

CREATE TABLE notifications (
            id INTEGER PRIMARY KEY AUTOINCREMENT, created_at INTEGER NOT NULL, workspace TEXT,
            kind TEXT NOT NULL, message TEXT NOT NULL, read INTEGER NOT NULL DEFAULT 0 CHECK(read IN (0,1))
        , space_id TEXT, subject TEXT, files TEXT NOT NULL DEFAULT '[]');

CREATE TABLE repository_resource_leases (
            lease_id TEXT PRIMARY KEY REFERENCES resource_leases(id) ON DELETE CASCADE,
            repository_id TEXT NOT NULL REFERENCES repositories(id), path TEXT NOT NULL
        );

CREATE TABLE checkpoints (
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
);

CREATE TABLE kept_branches (
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    branch TEXT NOT NULL,
    head TEXT NOT NULL,
    kept_at INTEGER NOT NULL,
    PRIMARY KEY(repository_id, branch)
);

CREATE TABLE forge_hosts (
    host TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    scheme TEXT NOT NULL
);

CREATE TABLE repository_agent_environments (
    repository_id TEXT NOT NULL,
    environment TEXT NOT NULL,
    PRIMARY KEY(repository_id, environment)
);

CREATE TABLE contribution_grace (
    repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
    cleanup_at INTEGER NOT NULL,
    claimed INTEGER NOT NULL CHECK(claimed IN (0, 1))
);

CREATE TABLE repository_moves (
    repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
    source TEXT NOT NULL,
    destination TEXT NOT NULL,
    source_id TEXT NOT NULL,
    destination_id TEXT NOT NULL,
    states TEXT NOT NULL,
    switched INTEGER NOT NULL DEFAULT 0 CHECK(switched IN (0, 1))
);

CREATE TABLE demos (
    execution_id TEXT PRIMARY KEY REFERENCES executions(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    record TEXT NOT NULL,
    has_run INTEGER NOT NULL DEFAULT 0 CHECK(has_run IN (0,1)),
    UNIQUE(workspace_id,name)
);

CREATE TABLE session_cleanup (
    id TEXT PRIMARY KEY,
    host TEXT NOT NULL,
    name TEXT NOT NULL,
    owner TEXT NOT NULL
, origin TEXT);

CREATE TABLE execution_upstream_resets (
    execution_id TEXT PRIMARY KEY REFERENCES executions(id) ON DELETE CASCADE,
    resets_at INTEGER NOT NULL
);

CREATE TABLE execution_unretried_turns (
    execution_id TEXT PRIMARY KEY REFERENCES executions(id) ON DELETE CASCADE
);

CREATE TABLE "space_files" (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    space_id TEXT NOT NULL,
    space TEXT NOT NULL,
    parent_id TEXT,
    repository_id TEXT,
    source TEXT NOT NULL,
    name TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    bytes INTEGER NOT NULL,
    created_at INTEGER NOT NULL
, attached_by TEXT, title TEXT, root_dir TEXT);

CREATE TABLE review_findings (
    review_id INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    number INTEGER NOT NULL CHECK(number > 0),
    severity TEXT NOT NULL CHECK(severity IN ('blocker','should-fix','nit')),
    path TEXT NOT NULL,
    line INTEGER NOT NULL CHECK(line > 0),
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    -- The JSON outcome the workspace's agent resolved it with.
    outcome TEXT,
    check_result TEXT CHECK(check_result IN ('confirmed','still-present')),
    checked_by INTEGER,
    PRIMARY KEY(review_id, number),
    CHECK((check_result IS NULL) = (checked_by IS NULL))
);

CREATE TABLE review_replies (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    review_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    body TEXT NOT NULL,
    FOREIGN KEY(review_id, number) REFERENCES review_findings(review_id, number) ON DELETE CASCADE
);

CREATE TABLE "reviews" (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    -- The JSON target: the workspace, or a PR and whether to post there.
    target TEXT NOT NULL,
    agent TEXT NOT NULL,
    -- The JSON model and effort asked for the agent, when any.
    model TEXT,
    reviewer TEXT,
    state TEXT NOT NULL CHECK(state IN ('queued','running','finished','failed','cancelled')),
    failure TEXT CHECK(failure IN ('usage-limit','quota-exhausted','rate-limit',
        'authentication-failed','execution-failed','invalid-output','timed-out',
        'invalid-false-positives')),
    head TEXT,
    base TEXT,
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    ended_at INTEGER,
    summary TEXT,
    verdict TEXT CHECK(verdict IN ('approve','comment','request_changes')),
    -- The JSON result of posting a PR review.
    posted TEXT,
    -- The workspace whose process cancelled the review, whose agents are
    -- not told about it.
    cancelled_by TEXT,
    -- Whether protection stopped its run, so it waits for healthy readings
    -- before it runs again, through a daemon restart too.
    waits_for_recovery INTEGER NOT NULL DEFAULT 0, session TEXT,
    CHECK((state='failed') = (failure IS NOT NULL))
);

CREATE TABLE "pr_cleanup" (
    space_id TEXT PRIMARY KEY,
    record TEXT NOT NULL
);

CREATE TABLE space_items (
    space_id TEXT NOT NULL,
    relation TEXT NOT NULL CHECK(relation IN ('opened','related')),
    kind TEXT NOT NULL CHECK(kind IN ('issue','pr','ci')),
    url TEXT NOT NULL,
    title TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY(space_id,relation,url)
);

CREATE TABLE "pr_activity" (
    space_id TEXT NOT NULL,
    url TEXT NOT NULL,
    record TEXT NOT NULL,
    PRIMARY KEY(space_id,url)
);

CREATE TABLE space_item_activity (
    space_id TEXT NOT NULL,
    url TEXT NOT NULL,
    record TEXT NOT NULL,
    PRIMARY KEY(space_id,url)
);

CREATE TABLE space_review (
    space_id TEXT NOT NULL,
    url TEXT NOT NULL,
    kind TEXT CHECK(kind IN ('issue','pr')),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY(space_id,url),
    CHECK((url='') = (kind IS NULL))
);

CREATE TABLE pending_updates (
    space_id TEXT NOT NULL,
    url TEXT NOT NULL,
    updates TEXT NOT NULL,
    PRIMARY KEY(space_id,url)
);

CREATE TABLE changed_on_both_sides (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    target TEXT NOT NULL,
    target_commit TEXT NOT NULL,
    head TEXT NOT NULL,
    files TEXT NOT NULL
);

CREATE TABLE cleanup_grace (
    space_id TEXT PRIMARY KEY,
    cleanup_at INTEGER NOT NULL,
    claimed INTEGER NOT NULL CHECK(claimed IN (0, 1))
);

CREATE TABLE space_parents (
    space_id TEXT PRIMARY KEY,
    parent_id TEXT NOT NULL
);

CREATE TABLE "agent_queue" (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK(kind IN ('launch','restore','job')),
    repository_id TEXT REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    agent TEXT NOT NULL,
    issue_title TEXT,
    plan TEXT,
    env TEXT,
    execution_id TEXT UNIQUE,
    state TEXT NOT NULL CHECK(state IN ('queued','starting')),
    created_at INTEGER NOT NULL,
    seen_at INTEGER,
    space_id TEXT,
    opens_workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    base_branch TEXT,
    automatic INTEGER NOT NULL DEFAULT 0 CHECK(automatic IN (0,1)),
    job TEXT, mode TEXT, narrowing TEXT, scope_space_id TEXT, priority INTEGER CHECK(priority BETWEEN 0 AND 100), stop_reason TEXT, wakes_when TEXT, issue_labels TEXT, source TEXT, starts_at_once INTEGER NOT NULL DEFAULT 0,
    CHECK(CASE kind
        WHEN 'launch' THEN plan IS NOT NULL AND env IS NOT NULL AND execution_id IS NULL AND job IS NULL
        WHEN 'restore' THEN plan IS NULL AND execution_id IS NOT NULL AND space_id IS NOT NULL AND job IS NULL
        ELSE plan IS NULL AND env IS NULL AND execution_id IS NULL AND job IS NOT NULL
    END)
);

CREATE TABLE ci_passed_jobs (
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    handle TEXT NOT NULL,
    PRIMARY KEY(repository_id, handle)
);

CREATE TABLE "runners" (
    repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
    forge_repository TEXT NOT NULL,
    forge_id INTEGER,
    uuid TEXT,
    token TEXT,
    CHECK((forge_id IS NULL) = (uuid IS NULL) AND (uuid IS NULL) = (token IS NULL))
);

CREATE TABLE merges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    default_branch TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    workspace TEXT NOT NULL,
    branch TEXT NOT NULL,
    source_commit TEXT NOT NULL,
    previous_commit TEXT NOT NULL,
    result_commit TEXT NOT NULL,
    merged_at INTEGER NOT NULL
);

CREATE TABLE space_agent_state (
    space_id TEXT PRIMARY KEY,
    state TEXT NOT NULL CHECK(state IN ('working','waiting','idle')),
    execution_id TEXT REFERENCES executions(id) ON DELETE CASCADE,
    since INTEGER NOT NULL,
    prompt_cache_expires_at INTEGER
, turn INTEGER NOT NULL DEFAULT 0, interrupted INTEGER NOT NULL DEFAULT 0);

CREATE TABLE journal_pruned (id INTEGER NOT NULL);

CREATE TABLE space_progress (
    space_id TEXT PRIMARY KEY,
    record TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    published_at INTEGER
);

CREATE TABLE "ci_jobs" (
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

CREATE TABLE "ci_job_usage" (
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    workflow TEXT NOT NULL,
    name TEXT NOT NULL,
    ended_at INTEGER NOT NULL,
    peak_memory INTEGER NOT NULL
);

CREATE TABLE "owned_places" (
            host TEXT NOT NULL,
            server TEXT NOT NULL,
            place TEXT NOT NULL,
            space_id TEXT NOT NULL,
            origin TEXT NOT NULL,
            PRIMARY KEY (host, server, place)
        );

CREATE TABLE space_conversions (
    space_id TEXT PRIMARY KEY,
    record TEXT NOT NULL
);

CREATE TABLE "workspace_renames" (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    branch TEXT NOT NULL
);

CREATE TABLE started_launches (
    entry_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    started_at INTEGER NOT NULL
);

CREATE TABLE space_completion (
    space_id TEXT PRIMARY KEY,
    record TEXT NOT NULL,
    cause TEXT NOT NULL DEFAULT 'manual'
);

CREATE TABLE space_sleep (
    space_id TEXT PRIMARY KEY,
    sleep TEXT NOT NULL CHECK(sleep IN ('turn','permanent'))
);

CREATE TABLE "agent_queue_blockers" (
    entry_id TEXT NOT NULL REFERENCES agent_queue(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('space','launch','issue','pr','ci_run','setup')),
    target TEXT NOT NULL,
    PRIMARY KEY(entry_id, kind, target)
);

CREATE TABLE space_turns (
    space_id TEXT PRIMARY KEY,
    turn INTEGER NOT NULL
);

CREATE TABLE agent_stops (
    execution_id TEXT PRIMARY KEY REFERENCES executions(id) ON DELETE CASCADE,
    reason TEXT NOT NULL,
    resumes_when TEXT NOT NULL,
    turn INTEGER);

CREATE TABLE space_priority (
    space_id TEXT PRIMARY KEY,
    priority INTEGER NOT NULL CHECK(priority BETWEEN 0 AND 100)
);

CREATE TABLE space_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    space_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    message TEXT NOT NULL,
    attachments TEXT NOT NULL DEFAULT '[]',
    wakes INTEGER NOT NULL DEFAULT 1 CHECK(wakes IN (0,1)),
    about TEXT,
    about_id TEXT,
    kind TEXT
);

CREATE TABLE space_issue (
    space_id TEXT NOT NULL,
    url TEXT NOT NULL,
    error TEXT,
    title TEXT, assignee TEXT, linked_by TEXT,
    PRIMARY KEY(space_id,url)
);

CREATE TABLE space_turn_retries (
    space_id TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    failure TEXT NOT NULL CHECK(failure IN ('rate_limited','overloaded','server_error')),
    attempt INTEGER NOT NULL CHECK(attempt > 0),
    due_at INTEGER NOT NULL,
    delivered INTEGER NOT NULL CHECK(delivered IN (0,1))
);

CREATE TABLE space_pr_titles (
    space_id TEXT NOT NULL,
    url TEXT NOT NULL,
    title TEXT NOT NULL,
    PRIMARY KEY(space_id,url)
);

CREATE TABLE issue_assignments (
    url TEXT NOT NULL,
    login TEXT NOT NULL COLLATE NOCASE,
    PRIMARY KEY(url, login)
);

CREATE TABLE removed_space_files (
    space_id TEXT PRIMARY KEY,
    chat INTEGER NOT NULL CHECK(chat IN (0,1)),
    expires_at INTEGER NOT NULL
);

CREATE TABLE gate_sessions (
    space_id TEXT NOT NULL,
    subject TEXT NOT NULL,
    agent TEXT NOT NULL,
    -- The JSON rules the session's opening gave it.
    rules TEXT NOT NULL,
    session TEXT NOT NULL,
    seen TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(space_id, subject)
);

CREATE TABLE gate_verdicts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    space_id TEXT NOT NULL,
    action TEXT NOT NULL,
    verdict TEXT NOT NULL CHECK(verdict IN ('allow','deny','escalate','failed')),
    reason TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE removed_spaces (
    space_id TEXT PRIMARY KEY,
    repository_id TEXT REFERENCES repositories(id) ON DELETE CASCADE,
    chat INTEGER NOT NULL CHECK(chat IN (0,1)),
    name TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    record TEXT NOT NULL
);

CREATE TABLE spaces (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('workspace','chat')),
    repository_id TEXT REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    number INTEGER UNIQUE,
    mode TEXT,
    narrowing TEXT, created_at INTEGER NOT NULL DEFAULT 0,
    CHECK(repository_id IS NOT NULL OR kind='chat')
);

CREATE TABLE "chats" (
    id TEXT PRIMARY KEY
        REFERENCES spaces(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    workspace_id TEXT REFERENCES workspaces(id),
    root_dir TEXT);

CREATE TABLE space_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    space_id TEXT,
    record TEXT NOT NULL,
    bulk INTEGER GENERATED ALWAYS AS (
        json_extract(record,'$.kind') IN ('agent_state','progress_changed')
        OR (json_extract(record,'$.kind')='item_activity'
            AND json_extract(record,'$.item_activity.update.kind')
                NOT IN ('closed','merged','reopened'))) VIRTUAL,
    CHECK (space_id IS json_extract(record,'$.space_id'))
);

CREATE TABLE space_holds (
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    reason TEXT,
    created_at INTEGER NOT NULL,
    placed_by TEXT NOT NULL CHECK(placed_by IN ('user','agent')),
    PRIMARY KEY(space_id,name)
);

CREATE TABLE "questions" (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    question TEXT NOT NULL,
    recommendation TEXT NOT NULL,
    reason TEXT,
    choices TEXT NOT NULL,
    answer TEXT,
    answered_at INTEGER,
    message_id INTEGER,
    closed_at INTEGER,
    number INTEGER NOT NULL DEFAULT 0,
    withdrawn INTEGER NOT NULL DEFAULT 0 CHECK(withdrawn IN (0,1)),
    in_chat INTEGER NOT NULL DEFAULT 0 CHECK(in_chat IN (0,1)),
    files TEXT NOT NULL DEFAULT '[]',
    space_number INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE "access_requests" (
    id TEXT PRIMARY KEY,
    space_id TEXT REFERENCES spaces(id) ON DELETE CASCADE,
    repository_id TEXT REFERENCES repositories(id) ON DELETE CASCADE,
    target_key TEXT NOT NULL,
    name TEXT NOT NULL,
    record TEXT NOT NULL,
    active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
    number INTEGER NOT NULL DEFAULT 0,
    space_number INTEGER NOT NULL DEFAULT 0,
    CHECK((space_id IS NULL) <> (repository_id IS NULL))
);

CREATE TABLE space_scopes (
    token TEXT PRIMARY KEY,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE
, agent INTEGER NOT NULL DEFAULT 0);

CREATE TABLE trigger_states (
    scope TEXT NOT NULL,
    name TEXT NOT NULL,
    paused INTEGER NOT NULL DEFAULT 0 CHECK(paused IN (0,1)),
    created_at INTEGER NOT NULL,
    counted_from INTEGER NOT NULL,
    last_fired_at INTEGER,
    PRIMARY KEY(scope, name)
);

CREATE TABLE trigger_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scope TEXT NOT NULL,
    name TEXT NOT NULL,
    at INTEGER NOT NULL,
    origin TEXT NOT NULL CHECK(origin IN ('source','manual')),
    outcome TEXT NOT NULL CHECK(outcome IN ('started','queued','skipped','duplicate','failed')),
    reason TEXT,
    space_id TEXT,
    queue_entry TEXT,
    launch TEXT
);

CREATE TABLE trigger_keys (
    scope TEXT NOT NULL,
    name TEXT NOT NULL,
    key TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY(scope, name, key)
);

CREATE TABLE container_projects (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    project TEXT NOT NULL UNIQUE,
    runtime TEXT NOT NULL CHECK(runtime IN ('docker','podman')),
    compose TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('starting','running','stopped','failed')),
    acquirer TEXT NOT NULL CHECK(acquirer IN ('user','agent','demo')),
    acquirer_execution TEXT CHECK((acquirer='user') = (acquirer_execution IS NULL)),
    for_agent INTEGER NOT NULL DEFAULT 0 CHECK(for_agent IN (0,1)),
    reason TEXT,
    error TEXT
);

CREATE TABLE agent_assigned_issues (url TEXT PRIMARY KEY);

CREATE TABLE "workspaces" (
    id TEXT PRIMARY KEY
        REFERENCES spaces(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    path TEXT NOT NULL UNIQUE,
    branch TEXT,
    state TEXT NOT NULL,
    error TEXT,
    base_commit TEXT,
    base_ref TEXT,
    git_dir TEXT,
    setup_finished INTEGER NOT NULL DEFAULT 0 CHECK(setup_finished IN (0,1)),
    observed_branch TEXT,
    base_workspace_id TEXT,
    active_at INTEGER,
    start_commit TEXT,
    made_for TEXT CHECK(made_for IS NULL OR start_commit IS NOT NULL)
        CHECK((branch IS NULL) <> (start_commit IS NULL)),
    setup_skipped INTEGER NOT NULL DEFAULT 0 CHECK(setup_skipped IN (0,1)),
    disposable INTEGER NOT NULL DEFAULT 0 CHECK(disposable IN (0,1)),
    copied_from TEXT,
    copied_from_checkpoint INTEGER,
    starting_checkpoint INTEGER,
    starting_index TEXT
);

CREATE TABLE merge_runs (
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    pr_url TEXT NOT NULL,
    merge_commit TEXT NOT NULL,
    branch TEXT NOT NULL,
    merged_at INTEGER NOT NULL,
    looking INTEGER NOT NULL DEFAULT 1 CHECK(looking IN (0,1)),
    runs TEXT NOT NULL DEFAULT '[]',
    ended INTEGER NOT NULL DEFAULT 0 CHECK(ended IN (0,1)),
    PRIMARY KEY(space_id,pr_url)
);

CREATE TABLE pending_worktrees (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    plan TEXT NOT NULL
);

CREATE TABLE space_wake_timers (
    space_id TEXT PRIMARY KEY,
    due_at INTEGER NOT NULL,
    message TEXT
);

CREATE TABLE execution_subagents (
    execution_id TEXT NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    starts INTEGER NOT NULL DEFAULT 0,
    stops INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (execution_id, agent_id)
);

CREATE TABLE execution_subagent_requests (
    execution_id TEXT PRIMARY KEY REFERENCES executions(id) ON DELETE CASCADE,
    at INTEGER NOT NULL
);

CREATE TABLE space_engagement (
    space_id TEXT PRIMARY KEY,
    interacted_at INTEGER,
    turn_ended_at INTEGER,
    previous_turn_ended_at INTEGER
);

CREATE TABLE reply_times (
    id INTEGER PRIMARY KEY,
    space_id TEXT NOT NULL,
    seconds INTEGER NOT NULL
);

CREATE TABLE waiting_preparations (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    steps TEXT NOT NULL,
    env TEXT,
    scope_space_id TEXT,
    for_agent INTEGER NOT NULL,
    waits TEXT NOT NULL
);

CREATE TABLE hub_identity (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    private_key BLOB NOT NULL
);

CREATE TABLE hub_members (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('machine','device')),
    name TEXT NOT NULL,
    public_key TEXT NOT NULL UNIQUE,
    joined_at INTEGER NOT NULL,
    version TEXT,
    last_seen INTEGER,
    revoked_at INTEGER
);

CREATE TABLE hub_membership (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    member_id TEXT NOT NULL,
    name TEXT NOT NULL,
    private_key BLOB NOT NULL,
    hub_key TEXT NOT NULL,
    url TEXT,
    connect TEXT,
    joined_at INTEGER NOT NULL,
    revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0,1)),
    CHECK((url IS NULL) <> (connect IS NULL))
);

CREATE TABLE removal_directories (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    identity TEXT NOT NULL
);

CREATE TABLE hub_spaces (
    space_id TEXT PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    summary TEXT NOT NULL,
    reported_at INTEGER NOT NULL
);

CREATE TABLE hub_space_copies (
    space_id TEXT NOT NULL,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    summary TEXT NOT NULL,
    since INTEGER NOT NULL,
    PRIMARY KEY(space_id, member_id)
);

CREATE TABLE hub_space_claims (
    space_id TEXT PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES hub_members(id)
);

CREATE TABLE hub_numbers (
    number INTEGER PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    space_id TEXT UNIQUE
);

CREATE TABLE hub_reserved_numbers (
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    number INTEGER NOT NULL,
    PRIMARY KEY(member_id, number)
);

CREATE TABLE hub_numbering (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    spares TEXT NOT NULL
);

CREATE TABLE hub_kept_numbers (
    space_id TEXT PRIMARY KEY,
    number INTEGER NOT NULL
);

CREATE UNIQUE INDEX repository_names ON repositories(name) WHERE name IS NOT NULL;

CREATE INDEX clean_requests_workspace ON simulator_clean_requests(workspace_id,id);

CREATE INDEX resource_lease_pool ON resource_leases(scope,pool);

CREATE INDEX notifications_unread ON notifications(read,id);

CREATE INDEX simulators_effective_owner ON simulators (
    coalesce(json_extract(record, '$.workspace_id'), json_extract(record, '$.last_workspace_id')),
    id
);

CREATE INDEX repository_resource_owner ON repository_resource_leases(repository_id);

CREATE INDEX reviews_workspace ON reviews(workspace_id, id);

CREATE UNIQUE INDEX reviews_active ON reviews(workspace_id)
    WHERE state IN ('queued','running');

CREATE INDEX review_replies_review ON review_replies(review_id, id);

CREATE INDEX space_parents_parent ON space_parents(parent_id);

CREATE INDEX agent_queue_opens ON agent_queue(opens_workspace_id);

CREATE UNIQUE INDEX agent_queue_job_handles ON agent_queue(repository_id, json_extract(job, '$.job.handle'))
    WHERE kind = 'job';

CREATE INDEX merges_by_branch ON merges(repository_id, branch);

CREATE INDEX space_agent_state_execution ON space_agent_state(execution_id);

CREATE INDEX ci_job_usage_jobs ON ci_job_usage(repository_id, workflow, name, ended_at);

CREATE INDEX agent_queue_scopes ON agent_queue(scope_space_id);

CREATE INDEX agent_queue_blocker_targets ON agent_queue_blockers(kind, target);

CREATE INDEX executions_space ON executions(space_id);

CREATE INDEX agent_queue_space ON agent_queue(space_id);

CREATE INDEX space_messages_space ON space_messages(space_id, id);

CREATE INDEX space_turn_retries_execution ON space_turn_retries(execution_id);

CREATE INDEX gate_verdicts_action ON gate_verdicts(space_id, action);

CREATE INDEX spaces_repository ON spaces(repository_id);

CREATE INDEX chats_workspace ON chats(workspace_id);

CREATE INDEX space_files_space ON space_files(space_id, id);

CREATE INDEX space_events_space ON space_events(space_id, bulk, id);

CREATE INDEX questions_space ON questions(space_id, id);

CREATE UNIQUE INDEX questions_number ON questions(space_number, number);

CREATE UNIQUE INDEX access_request_name
    ON access_requests(space_id,target_key,name) WHERE active=1;

CREATE INDEX access_request_target ON access_requests(space_id,target_key);

CREATE UNIQUE INDEX access_request_number ON access_requests(space_number, number);

CREATE INDEX access_request_repository ON access_requests(repository_id);

CREATE INDEX space_scopes_space ON space_scopes(space_id);

CREATE INDEX resource_lease_acquirers ON resource_leases(acquirer_execution)
    WHERE acquirer_execution IS NOT NULL;

CREATE INDEX trigger_runs_by_trigger ON trigger_runs(scope, name, id);

CREATE INDEX workspaces_base ON workspaces(base_workspace_id);

CREATE INDEX reply_times_space ON reply_times(space_id, id);

CREATE INDEX hub_spaces_member ON hub_spaces(member_id);

CREATE INDEX hub_numbers_member ON hub_numbers(member_id);

CREATE VIEW space_refs AS
    SELECT spaces.id, CASE spaces.kind
            WHEN 'workspace' THEN json_object('id',spaces.id,'name',spaces.name,
                'kind','workspace','branch',workspaces.branch)
            ELSE json_object('id',spaces.id,'name',spaces.name,'kind','chat') END AS ref
    FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace';

CREATE VIEW space_numbers AS
    SELECT number FROM spaces WHERE number IS NOT NULL
    UNION ALL SELECT space_number FROM access_requests WHERE space_id IS NULL;

CREATE VIEW free_space_number AS
    SELECT MIN(n) AS number FROM (SELECT 1 AS n UNION ALL SELECT number+1 FROM space_numbers)
        WHERE n NOT IN (SELECT number FROM space_numbers);

CREATE VIEW space_event_records AS
    SELECT spaces.id, spaces.kind AS space_kind, CASE spaces.kind
            WHEN 'workspace' THEN json_object('kind',NULL,'space_id',spaces.id,
                'space_kind','workspace','repository_id',spaces.repository_id,
                'name',spaces.name,'path',workspaces.path,'branch',workspaces.branch,
                'cause',NULL,'error',NULL)
            ELSE json_object('kind',NULL,'space_id',spaces.id,'space_kind','chat',
                'repository_id',spaces.repository_id,'name',spaces.name,
                'cause',NULL,'error',NULL) END AS record
    FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace';

INSERT INTO "sqlite_sequence" VALUES ('space_events', 0);

INSERT INTO "sqlite_sequence" VALUES ('review_replies', 0);

INSERT INTO "sqlite_sequence" VALUES ('agent_queue', 0);

INSERT INTO "sqlite_sequence" VALUES ('space_messages', 0);

INSERT INTO "sqlite_sequence" VALUES ('questions', 0);

INSERT INTO "sqlite_sequence" VALUES ('notifications', 0);

INSERT INTO "journal_pruned" VALUES (0);

CREATE TRIGGER simulator_owner_insert
BEFORE INSERT ON simulators
WHEN CASE
    WHEN NOT json_valid(NEW.record) THEN 1
    WHEN json_type(NEW.record) <> 'object' THEN 1
    ELSE EXISTS (
        SELECT key FROM json_each(NEW.record)
        WHERE key IN ('workspace_id', 'last_workspace_id')
        GROUP BY key HAVING count(*) > 1 OR sum(type NOT IN ('text', 'null')) > 0
    )
END
BEGIN
    SELECT RAISE(ABORT, 'invalid simulator ownership record: ' || NEW.id);
END;

CREATE TRIGGER simulator_owner_update
BEFORE UPDATE OF record ON simulators
WHEN CASE
    WHEN NOT json_valid(NEW.record) THEN 1
    WHEN json_type(NEW.record) <> 'object' THEN 1
    ELSE EXISTS (
        SELECT key FROM json_each(NEW.record)
        WHERE key IN ('workspace_id', 'last_workspace_id')
        GROUP BY key HAVING count(*) > 1 OR sum(type NOT IN ('text', 'null')) > 0
    )
END
BEGIN
    SELECT RAISE(ABORT, 'invalid simulator ownership record: ' || NEW.id);
END;

CREATE TRIGGER space_removed AFTER DELETE ON spaces BEGIN
    DELETE FROM space_parents WHERE space_id=OLD.id OR parent_id=OLD.id;
    DELETE FROM space_issue WHERE space_id=OLD.id;
    DELETE FROM pr_cleanup WHERE space_id=OLD.id;
    DELETE FROM space_items WHERE space_id=OLD.id;
    DELETE FROM pr_activity WHERE space_id=OLD.id;
    DELETE FROM space_item_activity WHERE space_id=OLD.id;
    DELETE FROM space_review WHERE space_id=OLD.id;
    DELETE FROM pending_updates WHERE space_id=OLD.id;
    DELETE FROM cleanup_grace WHERE space_id=OLD.id;
    DELETE FROM space_agent_state WHERE space_id=OLD.id;
    DELETE FROM space_progress WHERE space_id=OLD.id;
    DELETE FROM space_conversions WHERE space_id=OLD.id;
    DELETE FROM space_completion WHERE space_id=OLD.id;
    DELETE FROM space_turns WHERE space_id=OLD.id;
    DELETE FROM space_priority WHERE space_id=OLD.id;
    DELETE FROM space_sleep WHERE space_id=OLD.id;
    DELETE FROM space_messages WHERE space_id=OLD.id;
    DELETE FROM space_pr_titles WHERE space_id=OLD.id;
    DELETE FROM space_turn_retries WHERE space_id=OLD.id;
    DELETE FROM gate_sessions WHERE space_id=OLD.id;
    DELETE FROM gate_verdicts WHERE space_id=OLD.id;
    DELETE FROM executions WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE scope_space_id=OLD.id AND space_id IS NULL;
END;

CREATE TRIGGER space_agent_state_turn_kept AFTER DELETE ON space_agent_state
WHEN OLD.space_id IN (SELECT id FROM spaces) BEGIN
    INSERT INTO space_turns(space_id,turn) VALUES (OLD.space_id,OLD.turn)
        ON CONFLICT(space_id) DO UPDATE SET turn=max(turn,excluded.turn);
END;

CREATE TRIGGER space_events_retention AFTER INSERT ON space_events
WHEN NEW.bulk BEGIN
    UPDATE journal_pruned SET id=MAX(id, COALESCE((
        SELECT id FROM space_events WHERE space_id=NEW.space_id AND bulk
            ORDER BY id DESC LIMIT 1 OFFSET 500
    ), 0));
    DELETE FROM space_events WHERE space_id=NEW.space_id AND bulk AND id <= (
        SELECT id FROM space_events WHERE space_id=NEW.space_id AND bulk
            ORDER BY id DESC LIMIT 1 OFFSET 500
    );
END;

CREATE TRIGGER repository_triggers_removed AFTER DELETE ON repositories BEGIN
    DELETE FROM trigger_states WHERE scope=OLD.id;
    DELETE FROM trigger_runs WHERE scope=OLD.id;
    DELETE FROM trigger_keys WHERE scope=OLD.id;
END;

CREATE TRIGGER workspace_base_recorded AFTER UPDATE OF base_ref ON workspaces
WHEN OLD.base_ref IS NULL AND NEW.base_ref IS NOT NULL BEGIN
    UPDATE space_events SET record=json_set(record,'$.base_ref',NEW.base_ref)
        WHERE id=(SELECT MAX(id) FROM space_events
            WHERE space_id=NEW.id AND json_extract(record,'$.kind')='created');
END;

CREATE TRIGGER file_added AFTER INSERT ON space_files BEGIN
    INSERT INTO space_events(space_id,record) SELECT NEW.space_id, json_object(
        'kind','file_added','space_id',NEW.space_id,
        'repository_id',NEW.repository_id,
        'file',json_object('id',NEW.id,
            'space',json_object('id',NEW.space_id,'name',NEW.space,
                'kind',CASE WHEN chat THEN 'chat' ELSE 'workspace' END),
            'source',NEW.source,'name',NEW.name,'title',NEW.title,'path',NEW.path,
            'bytes',NEW.bytes,'created_at',NEW.created_at,
            'attached_by',json(NEW.attached_by))
    ) FROM (SELECT NEW.space_id IN (SELECT id FROM spaces WHERE kind='chat')
        OR NEW.space_id IN (SELECT space_id FROM removed_space_files WHERE chat) AS chat);
END;

CREATE TRIGGER file_removed AFTER DELETE ON space_files BEGIN
    INSERT INTO space_events(space_id,record) SELECT OLD.space_id, json_object(
        'kind','file_removed','space_id',OLD.space_id,
        'repository_id',OLD.repository_id,
        'file',json_object('id',OLD.id,
            'space',json_object('id',OLD.space_id,'name',OLD.space,
                'kind',CASE WHEN chat THEN 'chat' ELSE 'workspace' END),
            'source',OLD.source,'name',OLD.name,'title',OLD.title,'path',OLD.path,
            'bytes',OLD.bytes,'created_at',OLD.created_at,
            'attached_by',json(OLD.attached_by))
    ) FROM (SELECT OLD.space_id IN (SELECT id FROM spaces WHERE kind='chat')
        OR OLD.space_id IN (SELECT space_id FROM removed_space_files WHERE chat) AS chat);
END;

CREATE TRIGGER space_renamed AFTER UPDATE OF name ON spaces
WHEN NEW.name IS NOT OLD.name BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,'$.kind','renamed')
        FROM space_event_records WHERE id=NEW.id;
END;

CREATE TRIGGER space_converted AFTER UPDATE OF kind ON spaces
WHEN NEW.kind IS NOT OLD.kind BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,'$.kind','converted')
        FROM space_event_records WHERE id=NEW.id;
END;

CREATE TRIGGER space_leaving BEFORE DELETE ON spaces BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','child_removed',
        '$.child',json((SELECT ref FROM space_refs WHERE space_refs.id=OLD.id))
    ) FROM space_event_records
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=OLD.id);
END;

CREATE TRIGGER space_parent_set AFTER INSERT ON space_parents BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','parent_changed',
        '$.parent',json((SELECT ref FROM space_refs WHERE space_refs.id=NEW.parent_id))
    ) FROM space_event_records WHERE id=NEW.space_id AND space_kind='workspace';
END;

CREATE TRIGGER space_parent_changed AFTER UPDATE OF parent_id ON space_parents
WHEN NEW.parent_id IS NOT OLD.parent_id BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','parent_changed',
        '$.parent',json((SELECT ref FROM space_refs WHERE space_refs.id=NEW.parent_id))
    ) FROM space_event_records WHERE id=NEW.space_id AND space_kind='workspace';
END;

CREATE TRIGGER space_parent_cleared AFTER DELETE ON space_parents BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','parent_changed','$.parent',NULL
    ) FROM space_event_records WHERE id=OLD.space_id AND space_kind='workspace';
END;

CREATE TRIGGER space_review_ready AFTER INSERT ON space_review BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','review_ready',
        '$.review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''))
    ) FROM space_event_records WHERE id=NEW.space_id;
END;

CREATE TRIGGER space_review_ready_again AFTER UPDATE ON space_review BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','review_ready',
        '$.review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''))
    ) FROM space_event_records WHERE id=NEW.space_id;
END;

CREATE TRIGGER space_review_cleared AFTER DELETE ON space_review BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','review_cleared',
        '$.review',json_object('kind',OLD.kind,'url',nullif(OLD.url,''))
    ) FROM space_event_records WHERE id=OLD.space_id;
END;

CREATE TRIGGER space_agent_state_set AFTER INSERT ON space_agent_state BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','agent_state','$.agent_state',json_object('state',NEW.state)
    ) FROM space_event_records WHERE id=NEW.space_id;
END;

CREATE TRIGGER space_agent_state_changed AFTER UPDATE OF state ON space_agent_state
WHEN OLD.state IS NOT NEW.state BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','agent_state','$.agent_state',json_object('state',NEW.state)
    ) FROM space_event_records WHERE id=NEW.space_id;
END;

CREATE TRIGGER space_agent_state_cleared AFTER DELETE ON space_agent_state BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','agent_state','$.agent_state',json_object('state',NULL)
    ) FROM space_event_records WHERE id=OLD.space_id;
END;

CREATE TRIGGER space_completed AFTER INSERT ON space_completion BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','completed','$.cause',NEW.cause
    ) FROM space_event_records WHERE id=NEW.space_id;
END;

CREATE TRIGGER space_completed_again AFTER UPDATE OF record ON space_completion
WHEN json_extract(NEW.record,'$.error') IS NULL BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','completed','$.cause',NEW.cause
    ) FROM space_event_records WHERE id=NEW.space_id;
END;

CREATE TRIGGER space_undone AFTER DELETE ON space_completion BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,'$.kind','undone')
        FROM space_event_records WHERE id=OLD.space_id;
END;

CREATE TRIGGER demo_started AFTER UPDATE OF has_run ON demos
WHEN NEW.has_run AND NOT OLD.has_run BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','demo_started',
        '$.demo',json_object('name',NEW.name,'preset',json_extract(NEW.record,'$.preset'),
            'udid',json_extract(NEW.record,'$.environment.SHOAL_SIM_UDID'))
    ) FROM space_event_records WHERE id=NEW.workspace_id AND space_kind='workspace';
END;

CREATE TRIGGER demo_stopped AFTER DELETE ON demos WHEN OLD.has_run BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','demo_stopped',
        '$.demo',json_object('name',OLD.name,'preset',json_extract(OLD.record,'$.preset'),
            'udid',json_extract(OLD.record,'$.environment.SHOAL_SIM_UDID'))
    ) FROM space_event_records WHERE id=OLD.workspace_id AND space_kind='workspace';
END;

CREATE TRIGGER simulator_acquired AFTER UPDATE OF record ON simulators
WHEN json_extract(NEW.record,'$.state')='leased'
    AND json_extract(OLD.record,'$.state') IS NOT 'leased' BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','simulator_acquired',
        '$.simulator',json_object('name',json_extract(NEW.record,'$.lease_name'),
            'udid',json_extract(NEW.record,'$.udid'),
            'device',json_extract(NEW.record,'$.device'),
            'runtime',json_extract(NEW.record,'$.runtime'))
    ) FROM space_event_records
        WHERE id=json_extract(NEW.record,'$.workspace_id') AND space_kind='workspace';
END;

CREATE TRIGGER simulator_released AFTER UPDATE OF record ON simulators
WHEN json_extract(OLD.record,'$.state')='leased'
    AND json_extract(NEW.record,'$.state') IS NOT 'leased' BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','simulator_released',
        '$.simulator',json_object('name',json_extract(OLD.record,'$.lease_name'),
            'udid',json_extract(OLD.record,'$.udid'),
            'device',json_extract(OLD.record,'$.device'),
            'runtime',json_extract(OLD.record,'$.runtime'))
    ) FROM space_event_records
        WHERE id=json_extract(OLD.record,'$.workspace_id') AND space_kind='workspace';
END;

CREATE TRIGGER simulator_deleted AFTER DELETE ON simulators
WHEN json_extract(OLD.record,'$.state')='leased' BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','simulator_released',
        '$.simulator',json_object('name',json_extract(OLD.record,'$.lease_name'),
            'udid',json_extract(OLD.record,'$.udid'),
            'device',json_extract(OLD.record,'$.device'),
            'runtime',json_extract(OLD.record,'$.runtime'))
    ) FROM space_event_records
        WHERE id=json_extract(OLD.record,'$.workspace_id') AND space_kind='workspace';
END;

CREATE TRIGGER question_asked AFTER INSERT ON questions BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','question_asked',
        '$.question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,'answer',NULL)
    ) FROM space_event_records WHERE id=NEW.space_id;
END;

CREATE TRIGGER question_answered AFTER UPDATE OF answer ON questions
WHEN OLD.answer IS NULL AND NEW.answer IS NOT NULL BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','question_answered',
        '$.question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,
            'answer',NEW.answer)
    ) FROM space_event_records WHERE id=NEW.space_id;
END;

CREATE TRIGGER space_created AFTER INSERT ON spaces BEGIN
    UPDATE spaces SET number=(SELECT number FROM free_space_number)
        WHERE id=NEW.id AND number IS NULL;
    INSERT INTO space_events(space_id,record) SELECT NEW.id, json_set(record,
        '$.kind','created',
        '$.base_ref',workspaces.base_ref,
        '$.base_workspace',json((SELECT json_object('id',base.id,'name',base_space.name,
                'branch',base.branch)
            FROM workspaces AS base JOIN spaces AS base_space USING(id)
            WHERE base.id=workspaces.base_workspace_id)),
        '$.parent',json((SELECT ref FROM space_refs
            WHERE space_refs.id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id)))
    ) FROM space_event_records AS space
        LEFT JOIN workspaces ON workspaces.id=space.id AND space.space_kind='workspace'
        WHERE space.id=NEW.id;
    INSERT INTO space_events(space_id,record) SELECT NEW.id, json_set(record,'$.kind','ready')
        FROM space_event_records WHERE id=NEW.id
            AND id IN (SELECT id FROM workspaces WHERE state='ready') AND space_kind='workspace';
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','child_created',
        '$.child',json((SELECT ref FROM space_refs WHERE space_refs.id=NEW.id))
    ) FROM space_event_records
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id);
END;

CREATE TRIGGER workspace_base_removed BEFORE DELETE ON workspaces BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','base_changed','$.cause','removed',
        '$.base_workspace',json((SELECT json_object('id',base.id,'name',base_space.name,
                'branch',base.branch)
            FROM workspaces AS base JOIN spaces AS base_space USING(id)
            WHERE base.id=OLD.base_workspace_id))
    ) FROM space_event_records WHERE space_kind='workspace'
        AND id IN (SELECT id FROM workspaces WHERE base_workspace_id=OLD.id);
    UPDATE workspaces SET base_workspace_id=OLD.base_workspace_id WHERE base_workspace_id=OLD.id;
END;

CREATE TRIGGER workspace_branch_changed AFTER UPDATE OF observed_branch ON workspaces
WHEN NEW.observed_branch IS NOT OLD.observed_branch BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','branch_changed','$.branch',NEW.observed_branch
    ) FROM space_event_records WHERE id=NEW.id AND space_kind='workspace';
END;

CREATE TRIGGER workspace_branch_renamed AFTER UPDATE OF branch ON workspaces
WHEN NEW.branch IS NOT OLD.branch BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,'$.kind','renamed')
        FROM space_event_records WHERE id=NEW.id AND space_kind='workspace';
END;

CREATE TRIGGER workspace_ready AFTER UPDATE OF state ON workspaces
WHEN NEW.state='ready' AND OLD.state IN ('preparing','failed','reconciling')
    AND NEW.error IS NULL BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,'$.kind','ready')
        FROM space_event_records WHERE id=NEW.id AND space_kind='workspace';
END;

CREATE TRIGGER workspace_setup_failed AFTER UPDATE OF state ON workspaces
WHEN NEW.state='failed' AND OLD.state='preparing' BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','setup_failed','$.error',NEW.error
    ) FROM space_event_records WHERE id=NEW.id AND space_kind='workspace';
END;

CREATE TRIGGER space_wake_timer_removed AFTER DELETE ON spaces BEGIN
    DELETE FROM space_wake_timers WHERE space_id=OLD.id;
END;

CREATE TRIGGER space_engagement_removed AFTER DELETE ON spaces BEGIN
    DELETE FROM space_engagement WHERE space_id=OLD.id;
END;
