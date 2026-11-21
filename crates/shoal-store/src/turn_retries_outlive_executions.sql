-- A turn retry outlasts the execution it names while a restore of that
-- execution waits in the queue, so an agent Shoal put to sleep with a retry
-- pending is restored to continue once the retry is due. Without such a
-- restore, the execution's end still drops it, and a restore that leaves
-- the queue after its execution ended drops one it has not delivered. A
-- delivered retry stays until the turn it started ends, which a restore
-- the daemon starts runs in a new execution.
CREATE TABLE space_turn_retries_kept (
    space_id TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL,
    failure TEXT NOT NULL CHECK(failure IN ('rate_limited','overloaded','server_error')),
    attempt INTEGER NOT NULL CHECK(attempt > 0),
    due_at INTEGER NOT NULL,
    delivered INTEGER NOT NULL CHECK(delivered IN (0,1))
);
INSERT INTO space_turn_retries_kept(space_id,execution_id,failure,attempt,due_at,delivered)
    SELECT space_id,execution_id,failure,attempt,due_at,delivered FROM space_turn_retries;
DROP TABLE space_turn_retries;
-- The trigger that removes a space's retry with the space already names
-- what the rebuilt table becomes, so it is not rewritten for the rename.
PRAGMA legacy_alter_table=ON;
ALTER TABLE space_turn_retries_kept RENAME TO space_turn_retries;
PRAGMA legacy_alter_table=OFF;
CREATE INDEX space_turn_retries_execution ON space_turn_retries(execution_id);

CREATE TRIGGER space_turn_retry_execution_ended AFTER DELETE ON executions BEGIN
    DELETE FROM space_turn_retries WHERE execution_id=OLD.id
        AND NOT EXISTS (SELECT 1 FROM agent_queue WHERE execution_id=OLD.id);
END;

CREATE TRIGGER space_turn_retry_restore_ended AFTER DELETE ON agent_queue
WHEN OLD.execution_id IS NOT NULL BEGIN
    DELETE FROM space_turn_retries WHERE execution_id=OLD.execution_id AND delivered=0
        AND NOT EXISTS (SELECT 1 FROM executions WHERE id=OLD.execution_id);
END;
