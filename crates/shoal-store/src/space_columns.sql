-- A question, an approval request and a notification name their space,
-- a workspace or a chat, in one `space_id`, so a reader checks one column
-- whatever its kind and a conversion moves nothing. A question and a
-- request go with their space; a notification outlives it.

ALTER TABLE notifications RENAME COLUMN workspace_id TO space_id;

-- The views and triggers that read the columns that go come back once the
-- tables are rebuilt.
DROP VIEW free_space_number;
DROP VIEW space_numbers;
DROP TRIGGER question_asked;
DROP TRIGGER question_answered;

-- The last question ID is kept, so IDs are not reused.
CREATE TEMP TABLE kept_question_sequence AS
    SELECT seq FROM sqlite_sequence WHERE name='questions';
CREATE TABLE questions_rebuilt (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    question TEXT NOT NULL,
    recommendation TEXT NOT NULL,
    reason TEXT,
    choices TEXT NOT NULL,
    answer TEXT,
    answered_at INTEGER,
    message_id INTEGER,
    closed_at INTEGER,
    number INTEGER NOT NULL DEFAULT 0,
    withdrawn INTEGER NOT NULL DEFAULT 0 CHECK(withdrawn IN (0,1)),
    in_chat INTEGER NOT NULL DEFAULT 0 CHECK(in_chat IN (0,1)),
    files TEXT NOT NULL DEFAULT '[]',
    space_number INTEGER NOT NULL DEFAULT 0
);
INSERT INTO questions_rebuilt(id,space_id,created_at,question,recommendation,reason,choices,
        answer,answered_at,message_id,closed_at,number,withdrawn,in_chat,files,space_number)
    SELECT id,COALESCE(workspace_id,chat_id),created_at,question,recommendation,reason,choices,
        answer,answered_at,message_id,closed_at,number,withdrawn,in_chat,files,space_number
    FROM questions ORDER BY id;
DROP TABLE questions;

-- A request names its space, except a proposed issue, which belongs to its
-- repository.
CREATE TABLE access_requests_rebuilt (
    id TEXT PRIMARY KEY,
    space_id TEXT REFERENCES spaces(id) ON DELETE CASCADE,
    repository_id TEXT REFERENCES repositories(id) ON DELETE CASCADE,
    target_key TEXT NOT NULL,
    name TEXT NOT NULL,
    record TEXT NOT NULL,
    active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
    number INTEGER NOT NULL DEFAULT 0,
    space_number INTEGER NOT NULL DEFAULT 0,
    CHECK((space_id IS NULL) <> (repository_id IS NULL))
);
INSERT INTO access_requests_rebuilt(id,space_id,repository_id,target_key,name,record,active,
        number,space_number)
    SELECT id,COALESCE(workspace_id,chat_id),repository_id,target_key,name,record,active,
        number,space_number
    FROM access_requests ORDER BY rowid;
DROP TABLE access_requests;

-- The tables that keep their names are not rewritten for the renames, since
-- the views and triggers that name them come back below.
PRAGMA legacy_alter_table=ON;
ALTER TABLE questions_rebuilt RENAME TO questions;
ALTER TABLE access_requests_rebuilt RENAME TO access_requests;
PRAGMA legacy_alter_table=OFF;
DELETE FROM sqlite_sequence WHERE name='questions';
INSERT INTO sqlite_sequence(name,seq) SELECT 'questions',seq FROM kept_question_sequence;
DROP TABLE kept_question_sequence;

CREATE INDEX questions_space ON questions(space_id, id);
CREATE UNIQUE INDEX questions_number ON questions(space_number, number);
CREATE UNIQUE INDEX access_request_name
    ON access_requests(space_id,target_key,name) WHERE active=1;
CREATE INDEX access_request_target ON access_requests(space_id,target_key);
CREATE UNIQUE INDEX access_request_number ON access_requests(space_number, number);
CREATE INDEX access_request_repository ON access_requests(repository_id);

-- The space numbers taken, by spaces and by the issue proposals of
-- repositories.
CREATE VIEW space_numbers AS
    SELECT number FROM spaces WHERE number IS NOT NULL
    UNION ALL SELECT space_number FROM access_requests WHERE space_id IS NULL;
-- The smallest space number not taken.
CREATE VIEW free_space_number AS
    SELECT MIN(n) AS number FROM (SELECT 1 AS n UNION ALL SELECT number+1 FROM space_numbers)
        WHERE n NOT IN (SELECT number FROM space_numbers);

CREATE TRIGGER question_asked AFTER INSERT ON questions BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','question_asked','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,'answer',NULL)
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=NEW.space_id;
END;
CREATE TRIGGER question_answered AFTER UPDATE OF answer ON questions
WHEN OLD.answer IS NULL AND NEW.answer IS NOT NULL BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','question_answered','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,
            'answer',NEW.answer)
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=NEW.space_id;
END;
