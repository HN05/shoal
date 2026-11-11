-- Work a queued launch waits for: a workspace until it is done at its
-- current HEAD or removed, a launch until it opens its workspace, which
-- replaces it, and an issue until it closes, which deletes the row, or a
-- workspace that links it is done.
CREATE TABLE IF NOT EXISTS agent_queue_dependencies (
    entry_id TEXT NOT NULL REFERENCES agent_queue(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('workspace','launch','issue')),
    target TEXT NOT NULL,
    PRIMARY KEY(entry_id, kind, target)
);
CREATE INDEX IF NOT EXISTS agent_queue_dependency_targets ON agent_queue_dependencies(kind, target);
-- The branch of a completed dependency an admitted launch builds on.
ALTER TABLE agent_queue ADD COLUMN base_branch TEXT;
