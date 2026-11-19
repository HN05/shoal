-- SQLite maintains the expression from the JSON on every write, including
-- writes by older clients. ID is the second key to satisfy listing order.
CREATE INDEX IF NOT EXISTS simulators_effective_owner ON simulators (
    coalesce(json_extract(record, '$.workspace_id'), json_extract(record, '$.last_workspace_id')),
    id
);

-- Missing/null owners match serde Option<String>. Reject ambiguous or mistyped
-- ownership before it can become invisible to an indexed lookup.
CREATE TRIGGER IF NOT EXISTS simulator_owner_insert
BEFORE INSERT ON simulators
WHEN CASE
    WHEN NOT json_valid(NEW.record) THEN 1
    WHEN json_type(NEW.record) <> 'object' THEN 1
    ELSE EXISTS (
        SELECT key FROM json_each(NEW.record)
        WHERE key IN ('workspace_id', 'last_workspace_id')
        GROUP BY key HAVING count(*) > 1 OR sum(type NOT IN ('text', 'null')) > 0
    )
END
BEGIN
    SELECT RAISE(ABORT, 'invalid simulator ownership record: ' || NEW.id);
END;

CREATE TRIGGER IF NOT EXISTS simulator_owner_update
BEFORE UPDATE OF record ON simulators
WHEN CASE
    WHEN NOT json_valid(NEW.record) THEN 1
    WHEN json_type(NEW.record) <> 'object' THEN 1
    ELSE EXISTS (
        SELECT key FROM json_each(NEW.record)
        WHERE key IN ('workspace_id', 'last_workspace_id')
        GROUP BY key HAVING count(*) > 1 OR sum(type NOT IN ('text', 'null')) > 0
    )
END
BEGIN
    SELECT RAISE(ABORT, 'invalid simulator ownership record: ' || NEW.id);
END;
