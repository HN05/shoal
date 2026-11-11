-- Questions and approval requests are numbered within the space that asked:
-- both kinds share one sequence per space, so a number names one item.
DROP INDEX questions_number;
DROP INDEX access_request_number;
ALTER TABLE questions ADD COLUMN space_number INTEGER NOT NULL DEFAULT 0;
UPDATE questions SET space_number=COALESCE(
    (SELECT number FROM workspaces WHERE id=questions.workspace_id),
    (SELECT number FROM chats WHERE id=questions.chat_id));
-- A proposed issue names the space that proposed it in its record; one
-- whose space is gone keeps 0 until the migration reserves it a number.
ALTER TABLE access_requests ADD COLUMN space_number INTEGER NOT NULL DEFAULT 0;
UPDATE access_requests SET space_number=COALESCE(
    (SELECT number FROM workspaces WHERE id=access_requests.workspace_id),
    (SELECT number FROM workspaces
        WHERE id=CASE WHEN json_valid(record) THEN json_extract(record, '$.space_id') END),
    (SELECT number FROM chats
        WHERE id=CASE WHEN json_valid(record) THEN json_extract(record, '$.space_id') END),
    0);

-- A proposed issue keeps its space's number after the space goes, until it
-- is decided, so no new space takes the number it was announced with.
DROP VIEW free_space_number;
DROP VIEW space_numbers;
CREATE VIEW space_numbers AS
    SELECT number FROM workspaces WHERE number IS NOT NULL
    UNION ALL SELECT number FROM chats WHERE number IS NOT NULL
    UNION ALL SELECT space_number FROM access_requests WHERE workspace_id IS NULL;
CREATE VIEW free_space_number AS
    SELECT MIN(n) AS number FROM (SELECT 1 AS n UNION ALL SELECT number+1 FROM space_numbers)
        WHERE n NOT IN (SELECT number FROM space_numbers);
