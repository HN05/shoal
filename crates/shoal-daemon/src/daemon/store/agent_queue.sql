-- Agent launches waiting for a slot under `max_agents`, in insertion order.
-- `plan` is the CLI's opaque launch plan and `env` the launching terminal's
-- multiplexer variables. A starting entry keeps its slot until its agent
-- registers, its workspace goes, or its holder is gone for longer than the
-- grace period counted from `seen_at`.
CREATE TABLE agent_queue (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    agent TEXT NOT NULL,
    issue_title TEXT,
    plan TEXT NOT NULL,
    env TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('queued','starting')),
    created_at INTEGER NOT NULL,
    seen_at INTEGER,
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS agent_queue_workspace ON agent_queue(workspace_id);
