-- The preparation of a recorded workspace that waits before it creates the
-- worktree: for the blockers of the launch waiting to open it, on whose done
-- branch the worktree is created, or for disk space. `steps` is what the
-- preparation runs, `env` its environment and `scope_space_id` the space in
-- whose scope it runs, as JSON where they are; `for_agent` says an agent's
-- launch waited for it, and `waits` what it waits for now, as the workspace
-- shows it. The row goes once the preparation starts.
CREATE TABLE waiting_preparations (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    steps TEXT NOT NULL,
    env TEXT,
    scope_space_id TEXT,
    for_agent INTEGER NOT NULL,
    waits TEXT NOT NULL
);
