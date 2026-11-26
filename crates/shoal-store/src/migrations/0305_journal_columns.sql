-- The journal keeps what its queries filter on in columns the write path
-- fills from the typed event: its kind, whether it is bulk, the space's
-- repository and the child a child's creation or removal names. No query
-- reads a path inside a record. This migration is the one place that reads
-- them from the records written before.
CREATE TABLE space_events_columns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    space_id TEXT,
    kind TEXT NOT NULL,
    bulk INTEGER NOT NULL CHECK(bulk IN (0,1)),
    repository_id TEXT,
    child_id TEXT,
    record TEXT NOT NULL
);

INSERT INTO space_events_columns(id,created_at,space_id,kind,bulk,repository_id,child_id,record)
    SELECT id, created_at, space_id, json_extract(record,'$.kind'), bulk,
        json_extract(record,'$.repository_id'),
        CASE WHEN json_extract(record,'$.kind') IN ('child_created','child_removed')
            THEN json_extract(record,'$.child.id') END,
        record
    FROM space_events;

-- Event IDs keep increasing past those retention deleted.
DELETE FROM sqlite_sequence WHERE name='space_events_columns';
INSERT INTO sqlite_sequence(name,seq)
    SELECT 'space_events_columns', seq FROM sqlite_sequence WHERE name='space_events';

DROP TABLE space_events;
ALTER TABLE space_events_columns RENAME TO space_events;

CREATE INDEX space_events_space ON space_events(space_id, bulk, id);
CREATE INDEX space_events_kind ON space_events(space_id, kind, id);
CREATE INDEX space_events_child ON space_events(child_id) WHERE child_id IS NOT NULL;

CREATE TRIGGER space_events_retention AFTER INSERT ON space_events
WHEN NEW.bulk BEGIN
    UPDATE journal_pruned SET id=MAX(id, COALESCE((
        SELECT id FROM space_events WHERE space_id=NEW.space_id AND bulk
            ORDER BY id DESC LIMIT 1 OFFSET 500
    ), 0));
    DELETE FROM space_events WHERE space_id=NEW.space_id AND bulk AND id <= (
        SELECT id FROM space_events WHERE space_id=NEW.space_id AND bulk
            ORDER BY id DESC LIMIT 1 OFFSET 500
    );
END;
