-- What tests that emulate an earlier database run first to undo
-- space_completion.sql: a workspace's completion has its own table again,
-- and a chat's is a column of its record.
DROP TRIGGER space_completed; DROP TRIGGER space_completed_again;
DROP TRIGGER space_undone;
DROP TRIGGER workspace_completion_removed; DROP TRIGGER chat_completion_removed;
CREATE TABLE workspace_completion (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    record TEXT NOT NULL,
    cause TEXT NOT NULL DEFAULT 'manual'
);
INSERT INTO workspace_completion(workspace_id,record,cause)
    SELECT space_id,record,cause FROM space_completion
    WHERE space_id IN (SELECT id FROM workspaces);
ALTER TABLE chats ADD COLUMN completion TEXT;
UPDATE chats SET completion=(SELECT record FROM space_completion WHERE space_id=chats.id);
DROP TABLE space_completion;
CREATE TRIGGER workspace_completed AFTER INSERT ON workspace_completion BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','completed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NEW.cause,'error',NULL
    ) FROM workspaces WHERE id=NEW.workspace_id;
END;
CREATE TRIGGER workspace_completed_again AFTER UPDATE OF record ON workspace_completion
WHEN json_extract(NEW.record,'$.error') IS NULL BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','completed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NEW.cause,'error',NULL
    ) FROM workspaces WHERE id=NEW.workspace_id;
END;
CREATE TRIGGER workspace_undone AFTER DELETE ON workspace_completion BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','undone','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL
    ) FROM workspaces WHERE id=OLD.workspace_id;
END;
CREATE TRIGGER chat_completed AFTER UPDATE OF completion ON chats
WHEN NEW.completion IS NOT NULL BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','completed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause','manual','error',NULL
    ) FROM journal_spaces WHERE id=NEW.id;
END;
CREATE TRIGGER chat_undone AFTER UPDATE OF completion ON chats
WHEN OLD.completion IS NOT NULL AND NEW.completion IS NULL BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','undone','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL
    ) FROM journal_spaces WHERE id=NEW.id;
END;
