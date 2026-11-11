-- Questions and approval requests take the smallest number no stored one
-- holds, so the numbers users type stay short; the internal keys stay unique.
-- A question is deleted once its answer is delivered.
DELETE FROM questions WHERE answer IS NOT NULL
    AND (message_id IS NULL OR message_id NOT IN (SELECT id FROM agent_messages));
ALTER TABLE questions ADD COLUMN number INTEGER NOT NULL DEFAULT 0;
-- Existing questions keep their IDs as numbers, so waits running across the
-- upgrade resume them.
UPDATE questions SET number=id;
CREATE UNIQUE INDEX IF NOT EXISTS questions_number ON questions(number);
ALTER TABLE access_requests ADD COLUMN number INTEGER NOT NULL DEFAULT 0;
UPDATE access_requests SET number=(SELECT COUNT(*) FROM access_requests a WHERE a.rowid<=access_requests.rowid);
UPDATE access_requests SET record=json_set(record, '$.id', number) WHERE json_valid(record);
CREATE UNIQUE INDEX IF NOT EXISTS access_request_number ON access_requests(number);
UPDATE workspace_completion SET record=json_set(record, '$.keep_request',
        (SELECT number FROM access_requests WHERE id=json_extract(workspace_completion.record, '$.keep_request')))
    WHERE json_extract(record, '$.keep_request') IS NOT NULL;
