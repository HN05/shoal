-- A launch is also blocked by the setup of the workspace it opens, by ID,
-- after that setup failed, until a preparation there gets past its
-- post-setup hook, which deletes the row. Nothing references the table, so
-- it is rebuilt.
CREATE TABLE agent_queue_blockers_setup (
    entry_id TEXT NOT NULL REFERENCES agent_queue(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('space','launch','issue','pr','ci_run','setup')),
    target TEXT NOT NULL,
    PRIMARY KEY(entry_id, kind, target)
);
INSERT INTO agent_queue_blockers_setup(entry_id,kind,target)
    SELECT entry_id,kind,target FROM agent_queue_blockers ORDER BY rowid;
DROP TABLE agent_queue_blockers;
ALTER TABLE agent_queue_blockers_setup RENAME TO agent_queue_blockers;
CREATE INDEX agent_queue_blocker_targets ON agent_queue_blockers(kind, target);
