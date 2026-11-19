-- Every removed space, a workspace or a chat, has one removal record, which
-- `shoal restore` reopens it from until `expires_at`. `chat` says which kind
-- the space was, for what reads the record once its own row is gone; a chat
-- without a repository has none. Records that outlive their space but not
-- their repository.
CREATE TABLE removed_spaces (
    space_id TEXT PRIMARY KEY,
    repository_id TEXT REFERENCES repositories(id) ON DELETE CASCADE,
    chat INTEGER NOT NULL CHECK(chat IN (0,1)),
    name TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    record TEXT NOT NULL
);
INSERT INTO removed_spaces(space_id,repository_id,chat,name,expires_at,record)
    SELECT workspace_id,repository_id,0,name,expires_at,record FROM removed_workspaces
    ORDER BY rowid;
DROP TABLE removed_workspaces;
