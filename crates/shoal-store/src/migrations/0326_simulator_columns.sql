-- A simulator's state and owners, which queries, the owner index and the
-- journal's triggers read, are columns of their own and leave its record,
-- with its ID. The migration's preparation moved them into
-- `moved_simulators`. No trigger checks the owners' JSON types any more.
DROP TRIGGER simulator_owner_insert;
DROP TRIGGER simulator_owner_update;
DROP TRIGGER simulator_acquired;
DROP TRIGGER simulator_released;
DROP TRIGGER simulator_deleted;
DROP INDEX simulators_effective_owner;
DROP TABLE simulators;

CREATE TABLE simulators(
    id TEXT PRIMARY KEY, state TEXT NOT NULL, workspace_id TEXT, last_workspace_id TEXT,
    record TEXT NOT NULL
);

INSERT INTO simulators(id,state,workspace_id,last_workspace_id,record)
    SELECT id,state,workspace_id,last_workspace_id,record FROM temp.moved_simulators;
DROP TABLE temp.moved_simulators;

CREATE INDEX simulators_effective_owner ON simulators(coalesce(workspace_id, last_workspace_id), id);

CREATE TRIGGER simulator_acquired AFTER UPDATE OF state ON simulators
WHEN NEW.state='leased' AND OLD.state IS NOT 'leased' BEGIN
    SELECT journal_append('simulator_acquired', NEW.workspace_id,
        json_object('record', json(NEW.record)));
END;

CREATE TRIGGER simulator_released AFTER UPDATE OF state ON simulators
WHEN OLD.state='leased' AND NEW.state IS NOT 'leased' BEGIN
    SELECT journal_append('simulator_released', OLD.workspace_id,
        json_object('record', json(OLD.record)));
END;

CREATE TRIGGER simulator_deleted AFTER DELETE ON simulators
WHEN OLD.state='leased' BEGIN
    SELECT journal_append('simulator_released', OLD.workspace_id,
        json_object('record', json(OLD.record)));
END;
