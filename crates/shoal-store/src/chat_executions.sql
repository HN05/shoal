-- A chat's agent runs as an execution of the chat, so the chat no longer
-- records the process it ran in. Each chat whose agent ran or Shoal saved
-- is kept here until the daemon saves its agent for `shoal restore`, or
-- queues its restore when it ran or was saved to come back on its own; one
-- whose agent exited keeps its session as a workspace's does.
CREATE TABLE upgraded_chat_agents (
    chat_id TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
    saved INTEGER NOT NULL CHECK(saved IN (0,1)),
    automatic INTEGER NOT NULL CHECK(automatic IN (0,1)),
    stopped_by TEXT,
    turn TEXT
);
INSERT INTO upgraded_chat_agents(chat_id,saved,automatic,stopped_by,turn)
    SELECT id, restore IS NULL OR saved, restore IS NULL OR restore='automatic', stopped_by,
        (SELECT state FROM space_agent_state WHERE space_id=chats.id)
    FROM chats;
DELETE FROM space_agent_state WHERE space_id IN (SELECT id FROM chats);
-- A chat's agent logs its stops and restores as a workspace's does.
DELETE FROM workspace_events WHERE json_valid(record)
    AND json_extract(record,'$.kind') IN ('chat_saved','chat_restored');

DROP TRIGGER chat_agent_state_ended;
DROP TRIGGER chat_saved;
DROP TRIGGER chat_restored;
DROP TRIGGER chat_started;
DROP TRIGGER chat_ended;
DROP TRIGGER chat_renamed;
DROP TABLE chat_stopped_processes;
ALTER TABLE chats DROP COLUMN pid;
ALTER TABLE chats DROP COLUMN birth;
ALTER TABLE chats DROP COLUMN unique_id;
ALTER TABLE chats DROP COLUMN starting;
ALTER TABLE chats DROP COLUMN restore;
ALTER TABLE chats DROP COLUMN saved;
ALTER TABLE chats DROP COLUMN stopped_at;
ALTER TABLE chats DROP COLUMN stopped_by;

CREATE TRIGGER chat_started AFTER INSERT ON chats
WHEN NEW.id NOT IN (SELECT id FROM workspaces) BEGIN
    UPDATE chats SET number=(SELECT number FROM free_space_number)
        WHERE id=NEW.id AND number IS NULL;
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','chat_started','chat',json_object('id',id,'number',number,
            'repository_id',repository_id,'name',name,'agent',agent,
            'started_at',started_at)
    ) FROM chats WHERE id=NEW.id;
END;

CREATE TRIGGER chat_ended AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_ended','chat',json_object('id',OLD.id,'number',OLD.number,
            'repository_id',OLD.repository_id,'name',OLD.name,'agent',OLD.agent,
            'started_at',OLD.started_at)
    ));
END;

CREATE TRIGGER chat_renamed AFTER UPDATE OF name ON chats
WHEN NEW.name IS NOT OLD.name BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_renamed','chat',json_object('id',NEW.id,'repository_id',NEW.repository_id,
            'name',NEW.name,'agent',NEW.agent,'started_at',NEW.started_at)
    ));
END;
