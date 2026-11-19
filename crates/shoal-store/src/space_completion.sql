-- Each space's completion, a workspace's or a chat's: `record` holds what
-- `shoal done` recorded and `cause` what recorded it. It lasts until it is
-- withdrawn or the space goes. A conversion to the other kind withdraws
-- it, as the record of the old kind went with its row.
CREATE TABLE space_completion (
    space_id TEXT PRIMARY KEY,
    record TEXT NOT NULL,
    cause TEXT NOT NULL DEFAULT 'manual'
);
INSERT INTO space_completion(space_id,record,cause)
    SELECT workspace_id,record,cause FROM workspace_completion;
INSERT INTO space_completion(space_id,record,cause)
    SELECT id,completion,'manual' FROM chats WHERE completion IS NOT NULL;

CREATE TRIGGER workspace_completion_removed AFTER DELETE ON workspaces BEGIN
    DELETE FROM space_completion WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_completion_removed AFTER DELETE ON chats BEGIN
    DELETE FROM space_completion WHERE space_id=OLD.id;
END;

DROP TRIGGER workspace_completed;
DROP TRIGGER workspace_completed_again;
DROP TRIGGER workspace_undone;
DROP TABLE workspace_completion;
DROP TRIGGER chat_completed;
DROP TRIGGER chat_undone;
ALTER TABLE chats DROP COLUMN completion;

-- `shoal done` and its withdrawal; a repeated `done` records its completion
-- again, and recording a cleanup error records nothing. Removal deletes the
-- record after the space's row is gone, so only a withdrawal, explicit or by
-- a conversion, records `undone`.
CREATE TRIGGER space_completed AFTER INSERT ON space_completion BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','completed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NEW.cause,'error',NULL
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_completed_again AFTER UPDATE OF record ON space_completion
WHEN json_extract(NEW.record,'$.error') IS NULL BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','completed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NEW.cause,'error',NULL
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_undone AFTER DELETE ON space_completion BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','undone','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL
    ) FROM journal_spaces WHERE id=OLD.space_id;
END;
