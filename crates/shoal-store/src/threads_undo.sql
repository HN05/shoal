-- What tests that emulate an earlier database run first to undo
-- threads.sql: agents ask questions of the user.
DROP TRIGGER thread_opened;
DROP TRIGGER thread_posted;
DROP TRIGGER thread_resolved;
DROP TRIGGER thread_space_numbered;
DROP TRIGGER thread_space_restored;
DROP TRIGGER thread_space_removed;
DROP TRIGGER thread_removal_forgotten;
ALTER TABLE executions DROP COLUMN chat_thread;
ALTER TABLE executions ADD COLUMN asks_in_chat INTEGER NOT NULL DEFAULT 0 CHECK(asks_in_chat IN (0,1));
DROP TABLE thread_links;
DROP TABLE thread_holders;
DROP TABLE thread_posts;
DROP TABLE threads;
DELETE FROM sqlite_sequence WHERE name IN ('threads','thread_posts');
ALTER TABLE hub_escalated_threads RENAME TO hub_escalated_questions;
CREATE TABLE "questions" (
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
CREATE INDEX questions_space ON questions(space_id, id);
CREATE UNIQUE INDEX questions_number ON questions(space_number, number);
CREATE TRIGGER question_asked AFTER INSERT ON questions BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','question_asked',
        '$.question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,'answer',NULL)
    ) FROM space_event_records WHERE id=NEW.space_id;
END;

CREATE TRIGGER question_answered AFTER UPDATE OF answer ON questions
WHEN OLD.answer IS NULL AND NEW.answer IS NOT NULL BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','question_answered',
        '$.question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,
            'answer',NEW.answer)
    ) FROM space_event_records WHERE id=NEW.space_id;
END;
