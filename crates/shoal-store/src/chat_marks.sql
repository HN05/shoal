-- The mark an agent puts on the turn of the execution it runs as: once the
-- turn ends, its final message goes as a post in the thread `thread_id`
-- names, or as the alert `alert` describes, with the recipients and files
-- the command that marked it resolved. A turn has one mark, which goes with
-- its thread. `alerts.in_chat` says an alert carries a turn's final
-- message, which the user read once they prompt the agent of that chat.
CREATE TABLE chat_marks (
    execution_id TEXT PRIMARY KEY REFERENCES executions(id) ON DELETE CASCADE,
    thread_id INTEGER REFERENCES threads(id) ON DELETE CASCADE,
    alert TEXT,
    CHECK((thread_id IS NULL) <> (alert IS NULL))
);
INSERT INTO chat_marks(execution_id,thread_id)
    SELECT id,chat_thread FROM executions WHERE chat_thread IS NOT NULL;
ALTER TABLE executions DROP COLUMN chat_thread;
ALTER TABLE alerts ADD COLUMN in_chat INTEGER NOT NULL DEFAULT 0 CHECK(in_chat IN (0,1));
