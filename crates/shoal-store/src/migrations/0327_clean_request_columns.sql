-- A clean request's status, which quarantine filters and changes, is a
-- column of its own and leaves its record, with the workspace and request
-- ID the other columns hold. The migration's preparation moved them into
-- `moved_clean_requests`.
CREATE TABLE simulator_clean_requests_columns (
    id INTEGER PRIMARY KEY AUTOINCREMENT, request_id TEXT NOT NULL UNIQUE,
    workspace_id TEXT NOT NULL, status TEXT NOT NULL, record TEXT NOT NULL
);

INSERT INTO simulator_clean_requests_columns(id,request_id,workspace_id,status,record)
    SELECT old.id, old.request_id, old.workspace_id, moved.status, moved.record
    FROM simulator_clean_requests old JOIN temp.moved_clean_requests moved ON moved.id=old.id;
DROP TABLE temp.moved_clean_requests;

-- History entry IDs keep increasing past any deleted row's.
DELETE FROM sqlite_sequence WHERE name='simulator_clean_requests_columns';
INSERT INTO sqlite_sequence(name,seq)
    SELECT 'simulator_clean_requests_columns', seq FROM sqlite_sequence
    WHERE name='simulator_clean_requests';

DROP TABLE simulator_clean_requests;
ALTER TABLE simulator_clean_requests_columns RENAME TO simulator_clean_requests;

CREATE INDEX clean_requests_workspace ON simulator_clean_requests(workspace_id,id);
