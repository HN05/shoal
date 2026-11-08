-- The space each event belongs to, a workspace or a chat, which a space's
-- log reads by: the workspace or chat it records, or the chat a chat event
-- names.
ALTER TABLE workspace_events ADD COLUMN space_id TEXT GENERATED ALWAYS AS (COALESCE(
    json_extract(record,'$.workspace_id'), json_extract(record,'$.chat_id'),
    json_extract(record,'$.chat.id'))) VIRTUAL;
CREATE INDEX workspace_events_space ON workspace_events(space_id, id);

-- What an event of a space records about it: a workspace, or a chat, which
-- has no path or branch.
CREATE VIEW journal_spaces AS
    SELECT id, id AS workspace_id, NULL AS chat_id, repository_id, name, path, branch
        FROM workspaces
    UNION ALL SELECT id, NULL, id, repository_id, name, NULL, NULL FROM chats;

-- A chat's ready marks, turn states and completion reach the journal as a
-- workspace's do.
DROP TRIGGER space_review_ready;
CREATE TRIGGER space_review_ready AFTER INSERT ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''),'head',NEW.head)
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
DROP TRIGGER space_review_ready_again;
CREATE TRIGGER space_review_ready_again AFTER UPDATE ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''),'head',NEW.head)
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
-- Removal deletes a space's marks after its row is gone, so only explicit
-- withdrawal and unlinking record an event.
DROP TRIGGER space_review_cleared;
CREATE TRIGGER space_review_cleared AFTER DELETE ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_cleared','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'review',json_object('kind',OLD.kind,'url',nullif(OLD.url,''),'head',OLD.head)
    ) FROM journal_spaces WHERE id=OLD.space_id;
END;

DROP TRIGGER space_agent_state_set;
CREATE TRIGGER space_agent_state_set AFTER INSERT ON space_agent_state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,'agent_state',json_object('state',NEW.state)
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
-- Repeating a state is not a change.
DROP TRIGGER space_agent_state_changed;
CREATE TRIGGER space_agent_state_changed AFTER UPDATE OF state ON space_agent_state
WHEN OLD.state IS NOT NEW.state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,'agent_state',json_object('state',NEW.state)
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
-- Removal deletes a space's state after its row is gone, so only the end of
-- the reporting agent records a cleared state.
DROP TRIGGER space_agent_state_cleared;
CREATE TRIGGER space_agent_state_cleared AFTER DELETE ON space_agent_state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,'agent_state',json_object('state',NULL)
    ) FROM journal_spaces WHERE id=OLD.space_id;
END;

-- `shoal done` in a chat, and its withdrawal; a repeated `done` records its
-- completion again, as a workspace's does.
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
