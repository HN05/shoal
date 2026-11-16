-- Each workspace's Compose project, recorded before any `compose` call so a
-- project Shoal started always has a record, which goes with the workspace
-- only once `compose down` removed the project. The runtime is the one it
-- started with, so a changed machine config still stops and removes it.
-- Who acquired it decides how long it runs: the user's until released, an
-- agent's until the agent ends; `for_agent` marks one setup started for the
-- workspace's own agent, which takes it over when it starts.
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
