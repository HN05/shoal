-- What tests that emulate an earlier database run first to undo
-- space_holds.sql: only workspaces have holds.
DROP TABLE space_holds;
CREATE TABLE workspace_holds (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL, reason TEXT, created_at INTEGER NOT NULL,
    placed_by TEXT NOT NULL DEFAULT 'user' CHECK(placed_by IN ('user','agent')),
    PRIMARY KEY(workspace_id,name)
);
