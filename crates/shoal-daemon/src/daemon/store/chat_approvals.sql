-- A chat's agent asks for approval as a workspace's does: a request belongs
-- to its workspace or chat and goes with it, except a proposed issue, which
-- belongs to its repository.
CREATE UNIQUE INDEX IF NOT EXISTS access_request_chat_name
    ON access_requests(chat_id,target_key,name) WHERE active=1;
CREATE INDEX IF NOT EXISTS access_request_chat_target ON access_requests(chat_id,target_key);
DROP VIEW free_space_number;
DROP VIEW space_numbers;
CREATE VIEW space_numbers AS
    SELECT number FROM workspaces WHERE number IS NOT NULL
    UNION ALL SELECT number FROM chats WHERE number IS NOT NULL
    UNION ALL SELECT space_number FROM access_requests
        WHERE workspace_id IS NULL AND chat_id IS NULL;
CREATE VIEW free_space_number AS
    SELECT MIN(n) AS number FROM (SELECT 1 AS n UNION ALL SELECT number+1 FROM space_numbers)
        WHERE n NOT IN (SELECT number FROM space_numbers);
