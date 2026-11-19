-- The removed spaces, workspaces and chats alike, whose attached files stay
-- until `expires_at`. `chat` says which kind the space was, for what reads
-- its files once its own record is gone.
CREATE TABLE removed_space_files (
    space_id TEXT PRIMARY KEY,
    chat INTEGER NOT NULL CHECK(chat IN (0,1)),
    expires_at INTEGER NOT NULL
);

-- Removed chats keep their files as long as before, and removed workspaces
-- the files they kept for as long as their removal record; the files of a
-- workspace without one go on the next expiry pass, as before.
INSERT INTO removed_space_files(space_id,chat,expires_at)
    SELECT chat_id,1,expires_at FROM ended_chats;
INSERT OR IGNORE INTO removed_space_files(space_id,chat,expires_at)
    SELECT workspace_id,0,expires_at FROM removed_workspaces
    WHERE expires_at > unixepoch()
        AND workspace_id IN (SELECT space_id FROM workspace_files)
        AND workspace_id NOT IN (SELECT id FROM workspaces)
        AND workspace_id NOT IN (SELECT id FROM chats);
DROP TABLE ended_chats;

-- File events tell a removed workspace's files from a removed chat's by the
-- new record.
DROP TRIGGER file_added;
DROP TRIGGER file_removed;

CREATE TRIGGER file_added AFTER INSERT ON workspace_files BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','file_added',
        'workspace_id',CASE WHEN chat THEN NULL ELSE NEW.space_id END,
        'chat_id',CASE WHEN chat THEN NEW.space_id END,
        'repository_id',NEW.repository_id,
        'file',json_object('id',NEW.id,
            CASE WHEN chat THEN 'chat' ELSE 'workspace' END,NEW.space,
            'source',NEW.source,'name',NEW.name,'title',NEW.title,'path',NEW.path,
            'bytes',NEW.bytes,'created_at',NEW.created_at,
            'attached_by',json(NEW.attached_by))
    ) FROM (SELECT NEW.space_id IN (SELECT id FROM chats)
        OR NEW.space_id IN (SELECT space_id FROM removed_space_files WHERE chat) AS chat);
END;

CREATE TRIGGER file_removed AFTER DELETE ON workspace_files BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','file_removed',
        'workspace_id',CASE WHEN chat THEN NULL ELSE OLD.space_id END,
        'chat_id',CASE WHEN chat THEN OLD.space_id END,
        'repository_id',OLD.repository_id,
        'file',json_object('id',OLD.id,
            CASE WHEN chat THEN 'chat' ELSE 'workspace' END,OLD.space,
            'source',OLD.source,'name',OLD.name,'title',OLD.title,'path',OLD.path,
            'bytes',OLD.bytes,'created_at',OLD.created_at,
            'attached_by',json(OLD.attached_by))
    ) FROM (SELECT OLD.space_id IN (SELECT id FROM chats)
        OR OLD.space_id IN (SELECT space_id FROM removed_space_files WHERE chat) AS chat);
END;
