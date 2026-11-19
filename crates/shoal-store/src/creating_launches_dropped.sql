-- A terminal's `shoal start` queued launches whose plan created their
-- workspace once admitted. The agent queue now starts agents only in a
-- workspace their start recorded, so such a launch goes, each telling the
-- user to start it again. A chat's launch, whose plan starts its chat, and
-- one that opens a recorded workspace stay.
CREATE TEMP TABLE creating_launches AS
    SELECT id, name FROM agent_queue
    WHERE kind = 'launch' AND opens_workspace_id IS NULL AND space_id IS NULL
        AND NOT (json_valid(plan) AND json_extract(plan, '$.chat_start') IS NOT NULL);
INSERT INTO notifications(created_at, kind, message)
    SELECT CAST(strftime('%s', 'now') AS INTEGER), 'queue_failed',
        name || ' did not start from the agent queue: it waited to create its workspace, which this version of Shoal does when the start is made; start it again with shoal start'
    FROM creating_launches;
DELETE FROM agent_queue WHERE id IN (SELECT id FROM creating_launches);
DROP TABLE creating_launches;
