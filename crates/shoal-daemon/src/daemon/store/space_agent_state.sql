-- A chat's agent reports its turn state as a workspace's does, so the state
-- names the space, a workspace or chat. Only a workspace's states reach the
-- event journal, which records workspaces.
CREATE TABLE space_agent_state (
    space_id TEXT PRIMARY KEY,
    state TEXT NOT NULL CHECK(state IN ('working','waiting','idle')),
    execution_id TEXT REFERENCES executions(id) ON DELETE CASCADE,
    since INTEGER NOT NULL,
    prompt_cache_expires_at INTEGER
);
INSERT INTO space_agent_state(space_id,state,execution_id,since,prompt_cache_expires_at)
    SELECT workspace_id,state,execution_id,since,prompt_cache_expires_at
    FROM workspace_agent_state;
DROP TABLE workspace_agent_state;
CREATE INDEX space_agent_state_execution ON space_agent_state(execution_id);

CREATE TRIGGER space_agent_state_set AFTER INSERT ON space_agent_state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'agent_state',json_object('state',NEW.state)
    ) FROM workspaces WHERE id=NEW.space_id;
END;

-- Repeating a state is not a change.
CREATE TRIGGER space_agent_state_changed AFTER UPDATE OF state ON space_agent_state
WHEN OLD.state IS NOT NEW.state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'agent_state',json_object('state',NEW.state)
    ) FROM workspaces WHERE id=NEW.space_id;
END;

-- Workspace removal deletes its state after its row is gone, so only the
-- end of the reporting execution records a cleared state.
CREATE TRIGGER space_agent_state_cleared AFTER DELETE ON space_agent_state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'agent_state',json_object('state',NULL)
    ) FROM workspaces WHERE id=OLD.space_id;
END;

CREATE TRIGGER workspace_agent_state_removed AFTER DELETE ON workspaces BEGIN
    DELETE FROM space_agent_state WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_agent_state_removed AFTER DELETE ON chats BEGIN
    DELETE FROM space_agent_state WHERE space_id=OLD.id;
END;

-- A chat has no tracked execution whose end clears the state its agent
-- reported, so the state ends when the chat stops; a restored agent
-- reports its own.
CREATE TRIGGER chat_agent_state_ended AFTER UPDATE OF restore ON chats
WHEN OLD.restore IS NOT NEW.restore BEGIN
    DELETE FROM space_agent_state WHERE space_id=NEW.id;
END;
