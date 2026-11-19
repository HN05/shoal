-- Each space's progress as its agent last reported it: `record` holds the
-- status line and the plan's steps. `published_at` is when the progress
-- sections of the space's PRs came to show it, null after a change they do
-- not show yet, so a restarted daemon writes what an edit it delayed had
-- not. It lasts until the space goes.
CREATE TABLE space_progress (
    space_id TEXT PRIMARY KEY,
    record TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    published_at INTEGER
);

CREATE TRIGGER workspace_progress_removed AFTER DELETE ON workspaces BEGIN
    DELETE FROM space_progress WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_progress_removed AFTER DELETE ON chats BEGIN
    DELETE FROM space_progress WHERE space_id=OLD.id;
END;

-- A space's progress changes as often as its agent's turn, so its changes
-- are bulk events too.
DROP TRIGGER workspace_events_retention;
DROP INDEX workspace_events_space;
ALTER TABLE workspace_events DROP COLUMN bulk;
ALTER TABLE workspace_events ADD COLUMN bulk INTEGER GENERATED ALWAYS AS (
    json_extract(record,'$.kind') IN ('agent_state','progress_changed')
    OR (json_extract(record,'$.kind')='item_activity'
        AND json_extract(record,'$.item_activity.update.kind')
            NOT IN ('closed','merged','reopened'))) VIRTUAL;
CREATE INDEX workspace_events_space ON workspace_events(space_id, bulk, id);
CREATE TRIGGER workspace_events_retention AFTER INSERT ON workspace_events
WHEN NEW.bulk BEGIN
    UPDATE journal_pruned SET id=MAX(id, COALESCE((
        SELECT id FROM workspace_events WHERE space_id=NEW.space_id AND bulk
            ORDER BY id DESC LIMIT 1 OFFSET 500
    ), 0));
    DELETE FROM workspace_events WHERE space_id=NEW.space_id AND bulk AND id <= (
        SELECT id FROM workspace_events WHERE space_id=NEW.space_id AND bulk
            ORDER BY id DESC LIMIT 1 OFFSET 500
    );
END;
