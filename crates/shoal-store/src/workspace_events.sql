-- Events survive workspace removal and commit with the lifecycle transition.
CREATE TABLE workspace_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    record TEXT NOT NULL
);
ALTER TABLE workspace_completion ADD COLUMN cause TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE workspaces ADD COLUMN observed_branch TEXT;
UPDATE workspaces SET observed_branch=branch;

CREATE TRIGGER IF NOT EXISTS workspace_events_retention AFTER INSERT ON workspace_events BEGIN
    DELETE FROM workspace_events WHERE id <= (
        SELECT id FROM workspace_events ORDER BY id DESC LIMIT 1 OFFSET 1000
    );
END;

CREATE TRIGGER IF NOT EXISTS workspace_created AFTER INSERT ON workspaces BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','created','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL
    ));
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','ready','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL
    ) WHERE NEW.state='ready';
END;

CREATE TRIGGER IF NOT EXISTS workspace_ready AFTER UPDATE OF state ON workspaces
WHEN NEW.state='ready' AND OLD.state IN ('preparing','failed','reconciling') AND NEW.error IS NULL BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','ready','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL
    ));
END;

CREATE TRIGGER IF NOT EXISTS workspace_setup_failed AFTER UPDATE OF state ON workspaces
WHEN NEW.state='failed' AND OLD.state='preparing' BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','setup_failed','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NEW.error
    ));
END;

CREATE TRIGGER IF NOT EXISTS workspace_branch_changed AFTER UPDATE OF observed_branch ON workspaces
WHEN NEW.observed_branch IS NOT OLD.observed_branch BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','branch_changed','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.observed_branch,'cause',NULL,'error',NULL
    ));
END;

CREATE TRIGGER IF NOT EXISTS workspace_completed AFTER INSERT ON workspace_completion BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','completed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NEW.cause,'error',NULL
    ) FROM workspaces WHERE id=NEW.workspace_id;
END;

CREATE TRIGGER IF NOT EXISTS workspace_completed_again AFTER UPDATE OF record ON workspace_completion
WHEN json_extract(NEW.record,'$.error') IS NULL BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','completed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NEW.cause,'error',NULL
    ) FROM workspaces WHERE id=NEW.workspace_id;
END;

CREATE TRIGGER IF NOT EXISTS workspace_continued AFTER INSERT ON workspace_continuation BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','continued','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL
    ) FROM workspaces WHERE id=NEW.workspace_id;
END;
