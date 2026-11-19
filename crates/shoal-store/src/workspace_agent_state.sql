-- The turn state a workspace's agent last reported from its hooks. A state
-- reported from a tracked execution goes with that execution's record.
CREATE TABLE workspace_agent_state (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK(state IN ('working','waiting','idle')),
    execution_id TEXT REFERENCES executions(id) ON DELETE CASCADE,
    since INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS workspace_agent_state_execution
    ON workspace_agent_state(execution_id);

CREATE TRIGGER IF NOT EXISTS workspace_agent_state_set AFTER INSERT ON workspace_agent_state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'agent_state',json_object('state',NEW.state)
    ) FROM workspaces WHERE id=NEW.workspace_id;
END;

-- Repeating a state is not a change.
CREATE TRIGGER IF NOT EXISTS workspace_agent_state_changed AFTER UPDATE OF state ON workspace_agent_state
WHEN OLD.state IS NOT NEW.state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'agent_state',json_object('state',NEW.state)
    ) FROM workspaces WHERE id=NEW.workspace_id;
END;

-- Workspace removal cascades here after its row is gone, so only the end of
-- the reporting execution records a cleared state.
CREATE TRIGGER IF NOT EXISTS workspace_agent_state_cleared AFTER DELETE ON workspace_agent_state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'agent_state',json_object('state',NULL)
    ) FROM workspaces WHERE id=OLD.workspace_id;
END;
