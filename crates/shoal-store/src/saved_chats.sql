-- How a chat Shoal stopped with its session saved comes back; NULL while it
-- runs. A saved chat keeps its record, and with it its name, files and
-- messages, until it is restored or really ends.
ALTER TABLE chats ADD COLUMN restore TEXT CHECK(restore IN ('automatic','resume'));
-- The never-reused ID, as JSON, of the process a chat was saved while running
-- as, where the platform reports one, which finds what it started once it
-- ended.
ALTER TABLE chats ADD COLUMN unique_id TEXT;

CREATE TRIGGER chat_saved AFTER UPDATE OF restore ON chats
WHEN OLD.restore IS NULL AND NEW.restore IS NOT NULL BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_saved','chat',json_object('id',NEW.id,'repository_id',NEW.repository_id,
            'name',NEW.name,'agent',NEW.agent,'pid',NEW.pid,'started_at',NEW.started_at,
            'restore',NEW.restore)
    ));
END;

CREATE TRIGGER chat_restored AFTER UPDATE OF restore ON chats
WHEN OLD.restore IS NOT NULL AND NEW.restore IS NULL BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_restored','chat',json_object('id',NEW.id,'repository_id',NEW.repository_id,
            'name',NEW.name,'agent',NEW.agent,'pid',NEW.pid,'started_at',NEW.started_at)
    ));
END;
