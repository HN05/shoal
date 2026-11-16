-- What tests that emulate an earlier database run first to undo
-- chat_turn_marks.sql: a chat's row marks its agent's turn.
ALTER TABLE chats ADD COLUMN asks_in_chat INTEGER NOT NULL DEFAULT 0 CHECK(asks_in_chat IN (0,1));
