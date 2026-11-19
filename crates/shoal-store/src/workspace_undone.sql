-- Withdrawing a completion no longer defers cleanup; holds do. Workspaces kept
-- by the earlier continuation record keep that protection as a marked hold,
-- which the next explicit done releases as it did the continuation.
ALTER TABLE workspace_holds ADD COLUMN from_continuation INTEGER NOT NULL DEFAULT 0;
INSERT OR IGNORE INTO workspace_holds(workspace_id,name,reason,created_at,from_continuation)
    SELECT workspace_id,'continue','Kept by shoal continue',unixepoch(),1
    FROM workspace_continuation;
DROP TRIGGER workspace_continued;
DROP TABLE workspace_continuation;

-- Workspace removal cascades here after its row is gone, so only an explicit
-- undone records an event.
CREATE TRIGGER IF NOT EXISTS workspace_undone AFTER DELETE ON workspace_completion BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','undone','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL
    ) FROM workspaces WHERE id=OLD.workspace_id;
END;
