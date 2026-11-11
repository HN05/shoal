-- Ready-for-review marks: an agent's statement that linked work is ready at a
-- commit. The empty URL marks the workspace itself when nothing is linked.
CREATE TABLE workspace_review (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    url TEXT NOT NULL,
    kind TEXT CHECK(kind IN ('issue','pr')),
    head TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY(workspace_id,url),
    CHECK((url='') = (kind IS NULL))
);

CREATE TRIGGER IF NOT EXISTS workspace_review_ready AFTER INSERT ON workspace_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''),'head',NEW.head)
    ) FROM workspaces WHERE id=NEW.workspace_id;
END;

CREATE TRIGGER IF NOT EXISTS workspace_review_ready_again AFTER UPDATE ON workspace_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''),'head',NEW.head)
    ) FROM workspaces WHERE id=NEW.workspace_id;
END;

-- Workspace removal cascades here after its row is gone, so only explicit
-- withdrawal and unlinking record an event.
CREATE TRIGGER IF NOT EXISTS workspace_review_cleared AFTER DELETE ON workspace_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_cleared','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'review',json_object('kind',OLD.kind,'url',nullif(OLD.url,''),'head',OLD.head)
    ) FROM workspaces WHERE id=OLD.workspace_id;
END;
