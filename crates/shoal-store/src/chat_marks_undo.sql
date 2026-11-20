-- What tests that emulate an earlier database run first to undo
-- chat_marks.sql: an execution names the thread its turn posts in.
DROP TABLE chat_marks;
ALTER TABLE executions ADD COLUMN chat_thread INTEGER REFERENCES threads(id) ON DELETE SET NULL;
ALTER TABLE alerts DROP COLUMN in_chat;
