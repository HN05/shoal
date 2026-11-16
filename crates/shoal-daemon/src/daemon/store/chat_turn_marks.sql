-- A chat's agent marks a turn that asks in its conversation on the execution
-- it runs as, as a workspace's agent does: a chat's mark moves to its
-- running execution, and that of a chat that became a workspace to the
-- execution its conversion names, so a question asked in an open turn still
-- closes on the user's reply.
UPDATE executions SET asks_in_chat=1
    WHERE space_id IN (SELECT id FROM chats WHERE asks_in_chat) AND agent IS NOT NULL;
UPDATE executions SET asks_in_chat=1
    WHERE id IN (SELECT json_extract(record,'$.execution') FROM space_conversions
        WHERE json_extract(record,'$.to')='workspace'
            AND json_extract(record,'$.chat.asks_in_chat'));
UPDATE space_conversions SET record=json_remove(record,'$.chat.asks_in_chat');
ALTER TABLE chats DROP COLUMN asks_in_chat;
