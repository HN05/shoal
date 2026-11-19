-- A terminal's `shoal start` queued a chat's launch whose plan ran its
-- `shoal` arguments again once admitted. The queue now starts a waiting
-- chat from the start it was recorded with, so such a launch goes, each
-- telling the user to start it again.
CREATE TEMP TABLE chat_relaunches AS
    SELECT id, name FROM agent_queue
    WHERE kind = 'launch' AND json_valid(plan)
        AND json_extract(plan, '$.chat_start.relaunch') IS NOT NULL;
INSERT INTO notifications(created_at, kind, message)
    SELECT CAST(strftime('%s', 'now') AS INTEGER), 'queue_failed',
        name || ' did not start from the agent queue: this version of Shoal starts a waiting chat from the start it recorded, which its launch did not keep; start it again with shoal start'
    FROM chat_relaunches;
DELETE FROM agent_queue WHERE id IN (SELECT id FROM chat_relaunches);
DROP TABLE chat_relaunches;
