-- A child workspace was split from its parent, whose agent works on it or
-- coordinates its agent. Writers name a recorded workspace in the same
-- repository. Removal moved children up until migration 91 dropped
-- `workspace_parent_removed`; it now removes them first.
ALTER TABLE workspaces ADD COLUMN parent_workspace_id TEXT;
CREATE INDEX IF NOT EXISTS workspaces_parent ON workspaces(parent_workspace_id);

DROP TRIGGER workspace_created;
CREATE TRIGGER IF NOT EXISTS workspace_created AFTER INSERT ON workspaces BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','created','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL,
        'base_workspace',json((SELECT json_object('id',id,'name',name,'branch',branch)
            FROM workspaces WHERE id=NEW.base_workspace_id)),
        'parent_workspace',json((SELECT json_object('id',id,'name',name,'branch',branch)
            FROM workspaces WHERE id=NEW.parent_workspace_id))
    ));
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','ready','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL
    ) WHERE NEW.state='ready';
END;

-- Children of a removed workspace move up to its parent, or to none.
CREATE TRIGGER IF NOT EXISTS workspace_parent_removed BEFORE DELETE ON workspaces BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','parent_changed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause','removed','error',NULL,
        'parent_workspace',json((SELECT json_object('id',id,'name',name,'branch',branch)
            FROM workspaces WHERE id=OLD.parent_workspace_id))
    ) FROM workspaces WHERE parent_workspace_id=OLD.id;
    UPDATE workspaces SET parent_workspace_id=OLD.parent_workspace_id
        WHERE parent_workspace_id=OLD.id;
END;
