-- The daemon starts the restores of stopped agents itself, as it starts a
-- launch in an existing workspace: such a restore carries the variables of the
-- terminal that queued it, and its agent's execution has ended, so the entry
-- names the execution without referencing its row. A restore its waiting
-- wrapper carries out has none. Dropping the old table clears the launches'
-- dependencies, so they are kept aside and restored.
CREATE TEMP TABLE kept_queue_dependencies AS SELECT * FROM agent_queue_dependencies;

CREATE TABLE agent_queue_stopped (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK(kind IN ('launch','restore')),
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    agent TEXT NOT NULL,
    issue_title TEXT,
    plan TEXT,
    env TEXT,
    execution_id TEXT UNIQUE,
    state TEXT NOT NULL CHECK(state IN ('queued','starting')),
    created_at INTEGER NOT NULL,
    seen_at INTEGER,
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    opens_workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    base_branch TEXT,
    CHECK(CASE kind
        WHEN 'launch' THEN plan IS NOT NULL AND env IS NOT NULL AND execution_id IS NULL
        ELSE plan IS NULL AND execution_id IS NOT NULL AND workspace_id IS NOT NULL
    END)
);
INSERT INTO agent_queue_stopped(seq,id,kind,repository_id,name,agent,issue_title,plan,env,
        execution_id,state,created_at,seen_at,workspace_id,opens_workspace_id,base_branch)
    SELECT seq,id,kind,repository_id,name,agent,issue_title,plan,env,
        execution_id,state,created_at,seen_at,workspace_id,opens_workspace_id,base_branch
    FROM agent_queue;
DROP TABLE agent_queue;
ALTER TABLE agent_queue_stopped RENAME TO agent_queue;
CREATE INDEX agent_queue_workspace ON agent_queue(workspace_id);
CREATE INDEX agent_queue_opens ON agent_queue(opens_workspace_id);

INSERT INTO agent_queue_dependencies SELECT * FROM kept_queue_dependencies;
DROP TABLE kept_queue_dependencies;
