-- What tests that emulate an earlier database run first to undo the
-- pending_plans migration.
CREATE TABLE pending_worktrees (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    plan TEXT NOT NULL
);
INSERT INTO pending_worktrees(workspace_id,plan)
    SELECT id,pending_plan FROM workspaces WHERE pending_plan IS NOT NULL;
ALTER TABLE workspaces DROP COLUMN pending_plan;
