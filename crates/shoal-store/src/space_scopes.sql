-- Every scope token a space issues, which acts for it until revoked or the
-- space goes: a chat's, which its agent carries, and each environment a
-- workspace exported. A conversion changes the space's kind, not its tokens.
CREATE TABLE space_scopes (
    token TEXT PRIMARY KEY,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE
);
INSERT INTO space_scopes(token,space_id)
    SELECT token,workspace_id FROM workspace_scopes
    UNION SELECT scope_token,id FROM chats WHERE scope_token IS NOT NULL;
CREATE INDEX space_scopes_space ON space_scopes(space_id);
DROP TABLE workspace_scopes;
DROP INDEX chats_scope_token;
ALTER TABLE chats DROP COLUMN scope_token;
