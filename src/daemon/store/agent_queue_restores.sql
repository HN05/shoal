-- Automatic restores of protection-stopped agents wait in the agent queue in
-- one order with launches. A restore names its waiting execution instead of a
-- launch plan and leaves the queue with that execution.
CREATE TABLE agent_queue_typed (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK(kind IN ('launch','restore')),
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    agent TEXT NOT NULL,
    issue_title TEXT,
    plan TEXT,
    env TEXT,
    execution_id TEXT UNIQUE REFERENCES executions(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK(state IN ('queued','starting')),
    created_at INTEGER NOT NULL,
    seen_at INTEGER,
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    CHECK(CASE kind
        WHEN 'launch' THEN plan IS NOT NULL AND env IS NOT NULL AND execution_id IS NULL
        ELSE plan IS NULL AND env IS NULL AND execution_id IS NOT NULL AND workspace_id IS NOT NULL
    END)
);
INSERT INTO agent_queue_typed(seq,id,kind,repository_id,name,agent,issue_title,plan,env,state,created_at,seen_at,workspace_id)
    SELECT seq,id,'launch',repository_id,name,agent,issue_title,plan,env,state,created_at,seen_at,workspace_id
    FROM agent_queue;
DROP TABLE agent_queue;
ALTER TABLE agent_queue_typed RENAME TO agent_queue;
CREATE INDEX agent_queue_workspace ON agent_queue(workspace_id);
