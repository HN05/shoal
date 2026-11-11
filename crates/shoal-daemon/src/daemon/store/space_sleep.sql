-- How long a space's agent sleeps, during which Shoal does not wake it for
-- having stayed idle: until its next turn starts ('turn') or until
-- `shoal sleep --undo` ('permanent'). It lasts until the space goes, and
-- through its conversion to the other kind.
CREATE TABLE space_sleep (
    space_id TEXT PRIMARY KEY,
    sleep TEXT NOT NULL CHECK(sleep IN ('turn','permanent'))
);

CREATE TRIGGER workspace_sleep_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_sleep WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_sleep_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_sleep WHERE space_id=OLD.id;
END;
