-- A demo starts with its command's first run and stops when its record goes
-- with its execution, before a removed workspace's row; a demo whose command
-- never ran neither started nor stopped.
CREATE TRIGGER demo_started AFTER UPDATE OF has_run ON demos
WHEN NEW.has_run AND NOT OLD.has_run BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','demo_started','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'demo',json_object('name',NEW.name,'preset',json_extract(NEW.record,'$.preset'),
            'udid',json_extract(NEW.record,'$.environment.SHOAL_SIM_UDID'))
    ) FROM workspaces WHERE id=NEW.workspace_id;
END;

CREATE TRIGGER demo_stopped AFTER DELETE ON demos WHEN OLD.has_run BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','demo_stopped','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'demo',json_object('name',OLD.name,'preset',json_extract(OLD.record,'$.preset'),
            'udid',json_extract(OLD.record,'$.environment.SHOAL_SIM_UDID'))
    ) FROM workspaces WHERE id=OLD.workspace_id;
END;
