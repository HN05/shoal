-- The priority set on a space, from 0 to 100, which ranks its work in the
-- agent queue; a space without one has the default. It lasts until the
-- space goes, and through its conversion to the other kind.
CREATE TABLE space_priority (
    space_id TEXT PRIMARY KEY,
    priority INTEGER NOT NULL CHECK(priority BETWEEN 0 AND 100)
);

CREATE TRIGGER workspace_priority_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_priority WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_priority_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_priority WHERE space_id=OLD.id;
END;

-- The priority a launch was queued with, which the space it creates takes.
ALTER TABLE agent_queue ADD COLUMN priority INTEGER CHECK(priority BETWEEN 0 AND 100);
