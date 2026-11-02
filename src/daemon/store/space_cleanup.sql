-- Every space follows the same unattended removal: grace periods name a
-- workspace or a chat, whose IDs never collide, and go with either.
CREATE TEMP TABLE kept_grace AS SELECT * FROM cleanup_grace;
DROP TABLE cleanup_grace;
CREATE TABLE cleanup_grace (
    space_id TEXT PRIMARY KEY,
    cleanup_at INTEGER NOT NULL,
    claimed INTEGER NOT NULL CHECK(claimed IN (0, 1))
);
INSERT INTO cleanup_grace SELECT * FROM kept_grace;
DROP TABLE kept_grace;
CREATE TRIGGER IF NOT EXISTS workspace_grace_removed AFTER DELETE ON workspaces BEGIN
    DELETE FROM cleanup_grace WHERE space_id=OLD.id;
END;
CREATE TRIGGER IF NOT EXISTS chat_grace_removed AFTER DELETE ON chats BEGIN
    DELETE FROM cleanup_grace WHERE space_id=OLD.id;
END;

-- When a chat's agent last stopped, from which merged cleanup counts its
-- idle period across restarts; NULL while it runs. A chat already stopped
-- starts its idle period now.
ALTER TABLE chats ADD COLUMN stopped_at INTEGER;
-- Whether Shoal saved the stopped chat's session, as a shutdown or `shoal
-- daemon stop` does, rather than its agent exiting; only Shoal saved chats
-- before.
ALTER TABLE chats ADD COLUMN saved INTEGER NOT NULL DEFAULT 0 CHECK(saved IN (0,1));
UPDATE chats SET stopped_at=unixepoch(), saved=1 WHERE restore IS NOT NULL;
