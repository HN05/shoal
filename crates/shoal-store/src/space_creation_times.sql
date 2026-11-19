-- Every space records when it was created, which a conversion keeps: a
-- chat's start time moves there from its row, which a conversion replaced.
-- A recorded space takes the time its creation was journaled, else a chat's
-- start, else now.
ALTER TABLE spaces ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;
UPDATE spaces SET created_at=COALESCE(
    (SELECT MIN(created_at) FROM space_events
        WHERE space_id=spaces.id AND json_extract(record,'$.kind')='created'),
    (SELECT started_at FROM chats WHERE id=spaces.id),
    unixepoch());
ALTER TABLE chats DROP COLUMN started_at;
