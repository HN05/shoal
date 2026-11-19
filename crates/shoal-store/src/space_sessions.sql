-- A chat's agent keeps its sessions as a workspace's does, in its
-- executions' session files and its stopped agents and last sessions, which
-- the copy in the chat's row repeated. A chat that became a workspace keeps
-- its agent's session under the execution that agent runs as, and a removed
-- chat's record in its agents and last sessions.
UPDATE space_conversions SET record=json_set(record,'$.execution',(
        SELECT id FROM executions
            WHERE space_id=space_conversions.space_id AND agent IS NOT NULL
            ORDER BY rowid DESC LIMIT 1))
    WHERE json_extract(record,'$.to')='workspace' AND EXISTS (SELECT 1 FROM executions
        WHERE space_id=space_conversions.space_id AND agent IS NOT NULL);
UPDATE space_conversions SET record=json_remove(record,'$.chat.session');
UPDATE removed_spaces SET record=json_remove(record,'$.chat.session') WHERE chat;
ALTER TABLE chats DROP COLUMN session;
