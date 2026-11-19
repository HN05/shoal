-- What tests that emulate an earlier database run first to undo
-- agent_queue_blockers.sql: what blocks a queued launch is its dependencies
-- again, which earlier migrations rebuild.
CREATE TABLE agent_queue_dependencies (
    entry_id TEXT NOT NULL REFERENCES agent_queue(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('workspace','launch','issue')),
    target TEXT NOT NULL,
    PRIMARY KEY(entry_id, kind, target)
);
INSERT INTO agent_queue_dependencies(entry_id,kind,target)
    SELECT entry_id,CASE kind WHEN 'space' THEN 'workspace' ELSE kind END,target
    FROM agent_queue_blockers WHERE kind IN ('space','launch','issue');
DROP TABLE agent_queue_blockers;
CREATE INDEX agent_queue_dependency_targets ON agent_queue_dependencies(kind, target);
