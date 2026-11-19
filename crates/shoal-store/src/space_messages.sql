-- Unread messages for a space's agent, a workspace's or a chat's; delivery
-- deletes them. `space_id` names a space's record, so a conversion keeps
-- them under their IDs. `wakes` says whether a message starts an idle agent's
-- next turn; `about` and `about_id` name the space a message is about, by
-- its name when queued and its ID; `kind` is the notification the user
-- receives instead when the space goes before its agent reads it.
CREATE TABLE space_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    space_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    message TEXT NOT NULL,
    attachments TEXT NOT NULL DEFAULT '[]',
    wakes INTEGER NOT NULL DEFAULT 1 CHECK(wakes IN (0,1)),
    about TEXT,
    about_id TEXT,
    kind TEXT
);
CREATE INDEX space_messages_space ON space_messages(space_id, id);

-- Workspaces' messages keep their IDs and chats' move above every ID either
-- table gave, with the questions whose answers they carry; later messages
-- take IDs above those too, so an acknowledgement sent before the upgrade
-- deletes no newer message.
CREATE TEMP TABLE message_shift AS SELECT MAX(
    COALESCE((SELECT MAX(seq) FROM sqlite_sequence
        WHERE name IN ('agent_messages','chat_messages')), 0),
    COALESCE((SELECT MAX(id) FROM agent_messages), 0),
    COALESCE((SELECT MAX(id) FROM chat_messages), 0)
) AS shift;
INSERT INTO space_messages(id,space_id,created_at,message,attachments,wakes)
    SELECT id,workspace_id,created_at,message,attachments,wakes FROM agent_messages;
INSERT INTO space_messages(id,space_id,created_at,message,attachments,wakes,about,about_id,kind)
    SELECT id+shift,chat_id,created_at,message,attachments,1,NULLIF(workspace,''),workspace_id,kind
    FROM chat_messages, message_shift;
UPDATE questions SET message_id=message_id+(SELECT shift FROM message_shift)
    WHERE chat_id IS NOT NULL AND message_id IS NOT NULL;
DELETE FROM sqlite_sequence WHERE name='space_messages';
INSERT INTO sqlite_sequence(name,seq)
    SELECT 'space_messages', MAX(shift, COALESCE((SELECT MAX(id) FROM space_messages), 0))
    FROM message_shift;
DROP TABLE message_shift;
DROP TABLE agent_messages;
DROP TABLE chat_messages;

-- A space's messages go with its record, which a conversion keeps.
CREATE TRIGGER space_messages_removed AFTER DELETE ON space_records BEGIN
    DELETE FROM space_messages WHERE space_id=OLD.id;
END;
