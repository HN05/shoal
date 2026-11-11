-- A chat runs an agent in a repository's registered checkout, outside any
-- workspace. Its row lasts while the process with this identity lives;
-- `number` tells a repository's running chats apart.
CREATE TABLE chats (
    id TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    number INTEGER NOT NULL,
    agent TEXT NOT NULL,
    pid INTEGER NOT NULL,
    birth TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    UNIQUE(repository_id, number)
);

-- The running chat that opened the workspace; its end returns the workspace
-- to the user.
ALTER TABLE workspaces ADD COLUMN chat_id TEXT REFERENCES chats(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS workspaces_chat ON workspaces(chat_id);

CREATE TRIGGER IF NOT EXISTS chat_started AFTER INSERT ON chats BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_started','chat',json_object('id',NEW.id,'repository_id',NEW.repository_id,
            'number',NEW.number,'agent',NEW.agent,'pid',NEW.pid,'started_at',NEW.started_at)
    ));
END;

CREATE TRIGGER IF NOT EXISTS chat_ended AFTER DELETE ON chats BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_ended','chat',json_object('id',OLD.id,'repository_id',OLD.repository_id,
            'number',OLD.number,'agent',OLD.agent,'pid',OLD.pid,'started_at',OLD.started_at)
    ));
END;

CREATE TRIGGER IF NOT EXISTS workspace_chat_changed AFTER UPDATE OF chat_id ON workspaces
WHEN NEW.chat_id IS NOT OLD.chat_id BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_changed','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL,
        'chat',NEW.chat_id
    ));
END;
