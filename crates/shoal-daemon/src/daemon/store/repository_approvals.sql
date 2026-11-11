-- A proposed issue belongs to its repository and outlives the workspace that
-- proposed it, so its request names the repository instead of the workspace;
-- every other request still goes with its workspace.
CREATE TABLE access_requests_owned (
    id TEXT PRIMARY KEY,
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    target_key TEXT NOT NULL, name TEXT NOT NULL,
    record TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
    number INTEGER NOT NULL DEFAULT 0,
    repository_id TEXT REFERENCES repositories(id) ON DELETE CASCADE,
    CHECK((workspace_id IS NULL) <> (repository_id IS NULL))
);
INSERT INTO access_requests_owned(id,workspace_id,target_key,name,record,active,number)
    SELECT id,workspace_id,target_key,name,record,active,number FROM access_requests;
DROP TABLE access_requests;
ALTER TABLE access_requests_owned RENAME TO access_requests;
CREATE UNIQUE INDEX access_request_name
    ON access_requests(workspace_id,target_key,name) WHERE active=1;
CREATE INDEX access_request_target ON access_requests(workspace_id,target_key);
CREATE UNIQUE INDEX access_request_number ON access_requests(number);
CREATE INDEX access_request_repository ON access_requests(repository_id);
