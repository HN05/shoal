-- A chat's name replaces its number. Dropping the old table clears the
-- references to it, so the workspaces chats opened and the chats' unread
-- messages are kept aside and restored, without journal events.
DROP TRIGGER chat_started;
DROP TRIGGER chat_ended;
DROP TRIGGER workspace_chat_changed;
CREATE TEMP TABLE kept_workspace_chats AS
    SELECT id, chat_id FROM workspaces WHERE chat_id IS NOT NULL;
CREATE TEMP TABLE kept_chat_messages AS SELECT * FROM chat_messages;

CREATE TABLE chats_named (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL UNIQUE,
    agent TEXT NOT NULL,
    pid INTEGER NOT NULL,
    birth TEXT NOT NULL,
    started_at INTEGER NOT NULL
);
INSERT INTO chats_named(id,repository_id,name,agent,pid,birth,started_at)
    SELECT id,repository_id,name,agent,pid,birth,started_at FROM chats ORDER BY rowid;
DROP TABLE chats;
ALTER TABLE chats_named RENAME TO chats;

INSERT INTO chat_messages SELECT * FROM kept_chat_messages;
UPDATE workspaces SET chat_id=(SELECT chat_id FROM kept_workspace_chats AS kept
    WHERE kept.id=workspaces.id)
    WHERE id IN (SELECT id FROM kept_workspace_chats);
DROP TABLE kept_workspace_chats;
DROP TABLE kept_chat_messages;

CREATE TRIGGER chat_started AFTER INSERT ON chats BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_started','chat',json_object('id',NEW.id,'repository_id',NEW.repository_id,
            'name',NEW.name,'agent',NEW.agent,'pid',NEW.pid,'started_at',NEW.started_at)
    ));
END;

CREATE TRIGGER chat_ended AFTER DELETE ON chats BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_ended','chat',json_object('id',OLD.id,'repository_id',OLD.repository_id,
            'name',OLD.name,'agent',OLD.agent,'pid',OLD.pid,'started_at',OLD.started_at)
    ));
END;

CREATE TRIGGER workspace_chat_changed AFTER UPDATE OF chat_id ON workspaces
WHEN NEW.chat_id IS NOT OLD.chat_id BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_changed','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL,
        'chat',NEW.chat_id
    ));
END;
