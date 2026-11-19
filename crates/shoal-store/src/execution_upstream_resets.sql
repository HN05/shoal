-- When the upstream of an execution's proxy said it refuses requests until,
-- in Unix seconds, such as a usage limit's reset, which a retry of the
-- agent's turn waits past. It goes once the upstream answers again, or the
-- execution ends.
CREATE TABLE execution_upstream_resets (
    execution_id TEXT PRIMARY KEY REFERENCES executions(id) ON DELETE CASCADE,
    resets_at INTEGER NOT NULL
);
