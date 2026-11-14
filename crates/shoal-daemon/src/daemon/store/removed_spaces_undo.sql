-- What tests that emulate an earlier database run first to undo
-- removed_spaces.sql: only workspaces had removal records.
CREATE TABLE removed_workspaces (
    workspace_id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    record TEXT NOT NULL
);
INSERT INTO removed_workspaces(workspace_id,repository_id,name,expires_at,record)
    SELECT space_id,repository_id,name,expires_at,record FROM removed_spaces WHERE NOT chat
    ORDER BY rowid;
DROP TABLE removed_spaces;
