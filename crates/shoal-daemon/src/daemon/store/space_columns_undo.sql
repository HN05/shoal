-- What tests that emulate an earlier database run first to undo
-- space_columns.sql: questions and approval requests name a workspace or a
-- chat in a column of its kind, and notifications name either as a
-- workspace.
ALTER TABLE notifications RENAME COLUMN space_id TO workspace_id;

DROP VIEW free_space_number;
DROP VIEW space_numbers;
DROP TRIGGER question_asked;
DROP TRIGGER question_answered;

CREATE TABLE questions_undone (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    chat_id TEXT REFERENCES chats(id) ON DELETE CASCADE,
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
    in_chat INTEGER NOT NULL DEFAULT 0 CHECK(in_chat IN (0,1)), files TEXT NOT NULL DEFAULT '[]', space_number INTEGER NOT NULL DEFAULT 0,
    CHECK((workspace_id IS NULL) <> (chat_id IS NULL))
);
INSERT INTO questions_undone(id,workspace_id,chat_id,created_at,question,recommendation,reason,
        choices,answer,answered_at,message_id,closed_at,number,withdrawn,in_chat,files,
        space_number)
    SELECT id,(SELECT id FROM spaces WHERE id=space_id AND kind='workspace'),
        (SELECT id FROM spaces WHERE id=space_id AND kind='chat'),created_at,question,
        recommendation,reason,choices,answer,answered_at,message_id,closed_at,number,withdrawn,
        in_chat,files,space_number
    FROM questions;
DROP TABLE questions;

CREATE TABLE access_requests_undone (
    id TEXT PRIMARY KEY,
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    target_key TEXT NOT NULL, name TEXT NOT NULL,
    record TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
    number INTEGER NOT NULL DEFAULT 0,
    repository_id TEXT REFERENCES repositories(id) ON DELETE CASCADE, space_number INTEGER NOT NULL DEFAULT 0, chat_id TEXT REFERENCES chats(id) ON DELETE CASCADE,
    CHECK((workspace_id IS NOT NULL) + (chat_id IS NOT NULL) + (repository_id IS NOT NULL) = 1)
);
INSERT INTO access_requests_undone(id,workspace_id,target_key,name,record,active,number,
        repository_id,space_number,chat_id)
    SELECT id,(SELECT id FROM spaces WHERE id=space_id AND kind='workspace'),target_key,name,
        record,active,number,repository_id,space_number,
        (SELECT id FROM spaces WHERE id=space_id AND kind='chat')
    FROM access_requests;
DROP TABLE access_requests;

PRAGMA legacy_alter_table=ON;
ALTER TABLE questions_undone RENAME TO questions;
ALTER TABLE access_requests_undone RENAME TO access_requests;
PRAGMA legacy_alter_table=OFF;
CREATE INDEX questions_workspace ON questions(workspace_id, id);
CREATE INDEX questions_chat ON questions(chat_id, id);
CREATE UNIQUE INDEX questions_number ON questions(space_number, number);
CREATE UNIQUE INDEX access_request_name
    ON access_requests(workspace_id,target_key,name) WHERE active=1;
CREATE INDEX access_request_target ON access_requests(workspace_id,target_key);
CREATE UNIQUE INDEX access_request_chat_name
    ON access_requests(chat_id,target_key,name) WHERE active=1;
CREATE INDEX access_request_chat_target ON access_requests(chat_id,target_key);
CREATE UNIQUE INDEX access_request_number ON access_requests(space_number, number);
CREATE INDEX access_request_repository ON access_requests(repository_id);

CREATE VIEW space_numbers AS
    SELECT number FROM spaces WHERE number IS NOT NULL
    UNION ALL SELECT space_number FROM access_requests
        WHERE workspace_id IS NULL AND chat_id IS NULL;
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
        WHERE spaces.id=COALESCE(NEW.workspace_id,NEW.chat_id);
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
        WHERE spaces.id=COALESCE(NEW.workspace_id,NEW.chat_id);
END;
