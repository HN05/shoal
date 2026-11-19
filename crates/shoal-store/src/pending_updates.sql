-- The updates delivered from an item on a space's watch cursor that are not
-- acknowledged yet, kept apart from what the watch last saw of the item so
-- delivering them reads none of its history; a row only while some are.
CREATE TABLE pending_updates (
    space_id TEXT NOT NULL,
    url TEXT NOT NULL,
    updates TEXT NOT NULL,
    PRIMARY KEY(space_id,url)
);
INSERT INTO pending_updates(space_id,url,updates)
    SELECT space_id, url, json_extract(record, '$.pending') FROM pr_activity
    WHERE json_valid(record) AND json_array_length(record, '$.pending') > 0;
UPDATE pr_activity SET record=json_remove(record, '$.pending') WHERE json_valid(record);

CREATE TRIGGER workspace_pending_updates_removed AFTER DELETE ON workspaces BEGIN
    DELETE FROM pending_updates WHERE space_id=old.id;
END;
CREATE TRIGGER chat_pending_updates_removed AFTER DELETE ON chats BEGIN
    DELETE FROM pending_updates WHERE space_id=old.id;
END;
