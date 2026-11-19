-- What blocks a queued launch: a space until it is done at its current
-- HEAD or removed, a launch until it opens its workspace, which replaces
-- it, an issue until it closes, which deletes the row, or a workspace that
-- links it is done, a PR until it merges or closes, and a CI run until it
-- or a later run of its workflow on the default branch succeeds.
CREATE TABLE IF NOT EXISTS agent_queue_blockers (
    entry_id TEXT NOT NULL REFERENCES agent_queue(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('space','launch','issue','pr','ci_run')),
    target TEXT NOT NULL,
    PRIMARY KEY(entry_id, kind, target)
);
INSERT OR IGNORE INTO agent_queue_blockers(entry_id,kind,target)
    SELECT entry_id,CASE kind WHEN 'workspace' THEN 'space' ELSE kind END,target
    FROM agent_queue_dependencies;
DROP TABLE IF EXISTS agent_queue_dependencies;
CREATE INDEX IF NOT EXISTS agent_queue_blocker_targets ON agent_queue_blockers(kind, target);
