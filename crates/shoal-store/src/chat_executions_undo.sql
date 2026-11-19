-- What tests that emulate an earlier database run first to undo
-- chat_executions.sql: chats record the process their agent runs in.
DROP TABLE upgraded_chat_agents;
DROP TRIGGER chat_started;
DROP TRIGGER chat_ended;
DROP TRIGGER chat_renamed;
ALTER TABLE chats ADD COLUMN pid INTEGER NOT NULL DEFAULT 0;
ALTER TABLE chats ADD COLUMN birth TEXT NOT NULL DEFAULT '';
ALTER TABLE chats ADD COLUMN starting INTEGER NOT NULL DEFAULT 0 CHECK (starting IN (0,1));
ALTER TABLE chats ADD COLUMN restore TEXT CHECK(restore IN ('automatic','resume'));
ALTER TABLE chats ADD COLUMN unique_id TEXT;
ALTER TABLE chats ADD COLUMN stopped_at INTEGER;
ALTER TABLE chats ADD COLUMN saved INTEGER NOT NULL DEFAULT 0 CHECK(saved IN (0,1));
ALTER TABLE chats ADD COLUMN stopped_by TEXT;
CREATE TABLE chat_stopped_processes (
    chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    pid INTEGER NOT NULL,
    birth TEXT NOT NULL,
    unique_id TEXT,
    PRIMARY KEY(chat_id, pid, birth)
);
CREATE TRIGGER chat_agent_state_ended AFTER UPDATE OF restore ON chats
WHEN OLD.restore IS NOT NEW.restore BEGIN
    DELETE FROM space_agent_state WHERE space_id=NEW.id;
END;

CREATE TRIGGER chat_saved AFTER UPDATE OF restore ON chats
WHEN OLD.restore IS NULL AND NEW.restore IS NOT NULL BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_saved','chat',json_object('id',NEW.id,'repository_id',NEW.repository_id,
            'name',NEW.name,'agent',NEW.agent,'pid',NEW.pid,'started_at',NEW.started_at,
            'restore',NEW.restore)
    ));
END;

CREATE TRIGGER chat_restored AFTER UPDATE OF restore ON chats
WHEN OLD.restore IS NOT NULL AND NEW.restore IS NULL BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_restored','chat',json_object('id',NEW.id,'repository_id',NEW.repository_id,
            'name',NEW.name,'agent',NEW.agent,'pid',NEW.pid,'started_at',NEW.started_at)
    ));
END;

CREATE TRIGGER chat_started AFTER INSERT ON chats
WHEN NEW.id NOT IN (SELECT id FROM workspaces) BEGIN
    UPDATE chats SET number=(SELECT number FROM free_space_number)
        WHERE id=NEW.id AND number IS NULL;
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','chat_started','chat',json_object('id',id,'number',number,
            'repository_id',repository_id,'name',name,'agent',agent,'pid',pid,
            'started_at',started_at)
    ) FROM chats WHERE id=NEW.id;
END;

CREATE TRIGGER chat_ended AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_ended','chat',json_object('id',OLD.id,'number',OLD.number,
            'repository_id',OLD.repository_id,'name',OLD.name,'agent',OLD.agent,'pid',OLD.pid,
            'started_at',OLD.started_at)
    ));
END;

CREATE TRIGGER chat_renamed AFTER UPDATE OF name ON chats
WHEN NEW.name IS NOT OLD.name BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_renamed','chat',json_object('id',NEW.id,'repository_id',NEW.repository_id,
            'name',NEW.name,'agent',NEW.agent,'pid',NEW.pid,'started_at',NEW.started_at)
    ));
END;
