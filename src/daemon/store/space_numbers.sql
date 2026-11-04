-- Every space, a workspace or a chat, has a number no other space holds:
-- the smallest one free when it is recorded, kept across renames and freed
-- when it goes. Existing spaces are numbered in the order they were
-- recorded, workspaces first.
ALTER TABLE workspaces ADD COLUMN number INTEGER;
ALTER TABLE chats ADD COLUMN number INTEGER;
UPDATE workspaces SET number=(SELECT COUNT(*) FROM workspaces w WHERE w.rowid<=workspaces.rowid);
UPDATE chats SET number=(SELECT COUNT(*) FROM workspaces)
    + (SELECT COUNT(*) FROM chats c WHERE c.started_at<chats.started_at
        OR (c.started_at=chats.started_at AND c.rowid<=chats.rowid));
CREATE UNIQUE INDEX workspaces_number ON workspaces(number);
CREATE UNIQUE INDEX chats_number ON chats(number);

-- The space numbers taken.
CREATE VIEW space_numbers AS
    SELECT number FROM workspaces WHERE number IS NOT NULL
    UNION ALL SELECT number FROM chats WHERE number IS NOT NULL;
-- The smallest space number not taken.
CREATE VIEW free_space_number AS
    SELECT MIN(n) AS number FROM (SELECT 1 AS n UNION ALL SELECT number+1 FROM space_numbers)
        WHERE n NOT IN (SELECT number FROM space_numbers);

-- A space takes its number in the transaction that records it, whichever
-- writer records it. A chat takes it before its start is journaled, which
-- names it.
CREATE TRIGGER workspace_numbered AFTER INSERT ON workspaces WHEN NEW.number IS NULL BEGIN
    UPDATE workspaces SET number=(SELECT number FROM free_space_number) WHERE id=NEW.id;
END;
DROP TRIGGER chat_started;
CREATE TRIGGER chat_started AFTER INSERT ON chats BEGIN
    UPDATE chats SET number=(SELECT number FROM free_space_number)
        WHERE id=NEW.id AND number IS NULL;
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','chat_started','chat',json_object('id',id,'number',number,
            'repository_id',repository_id,'name',name,'agent',agent,'pid',pid,
            'started_at',started_at)
    ) FROM chats WHERE id=NEW.id;
END;
DROP TRIGGER chat_ended;
CREATE TRIGGER chat_ended AFTER DELETE ON chats BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_ended','chat',json_object('id',OLD.id,'number',OLD.number,
            'repository_id',OLD.repository_id,'name',OLD.name,'agent',OLD.agent,'pid',OLD.pid,
            'started_at',OLD.started_at)
    ));
END;
