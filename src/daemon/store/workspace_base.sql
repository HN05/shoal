-- A stacked workspace's branch builds on its base workspace's branch. Writers
-- name a recorded workspace in the same repository, and removal moves stacked
-- workspaces down before the row goes.
ALTER TABLE workspaces ADD COLUMN base_workspace_id TEXT;
CREATE INDEX IF NOT EXISTS workspaces_base ON workspaces(base_workspace_id);

DROP TRIGGER workspace_created;
CREATE TRIGGER IF NOT EXISTS workspace_created AFTER INSERT ON workspaces BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','created','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL,
        'base_workspace',json((SELECT json_object('id',id,'name',name,'branch',branch)
            FROM workspaces WHERE id=NEW.base_workspace_id))
    ));
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','ready','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL
    ) WHERE NEW.state='ready';
END;

-- Workspaces stacked on a removed one move down to its base, or to none.
CREATE TRIGGER IF NOT EXISTS workspace_base_removed BEFORE DELETE ON workspaces BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','base_changed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause','removed','error',NULL,
        'base_workspace',json((SELECT json_object('id',id,'name',name,'branch',branch)
            FROM workspaces WHERE id=OLD.base_workspace_id))
    ) FROM workspaces WHERE base_workspace_id=OLD.id;
    UPDATE workspaces SET base_workspace_id=OLD.base_workspace_id WHERE base_workspace_id=OLD.id;
END;
