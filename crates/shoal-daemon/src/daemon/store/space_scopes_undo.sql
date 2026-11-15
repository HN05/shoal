-- What tests that emulate an earlier database run first to undo
-- space_scopes.sql: a workspace's exported environments and a chat's token
-- each have their kind's place again.
CREATE TABLE workspace_scopes (
    token TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE
);
INSERT INTO workspace_scopes(token,workspace_id)
    SELECT token,space_id FROM space_scopes WHERE space_id IN (SELECT id FROM workspaces);
ALTER TABLE chats ADD COLUMN scope_token TEXT;
UPDATE chats SET scope_token=(SELECT token FROM space_scopes WHERE space_id=chats.id);
CREATE UNIQUE INDEX chats_scope_token ON chats(scope_token);
DROP TABLE space_scopes;
