-- What tests that emulate an earlier database run first to undo
-- alerts.sql: notifications and space messages keep apart.
CREATE TABLE notifications (
            id INTEGER PRIMARY KEY AUTOINCREMENT, created_at INTEGER NOT NULL, workspace TEXT,
            kind TEXT NOT NULL, message TEXT NOT NULL, read INTEGER NOT NULL DEFAULT 0 CHECK(read IN (0,1))
        , space_id TEXT, subject TEXT, files TEXT NOT NULL DEFAULT '[]');
CREATE INDEX notifications_unread ON notifications(read,id);
CREATE TABLE space_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    space_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    message TEXT NOT NULL,
    attachments TEXT NOT NULL DEFAULT '[]',
    wakes INTEGER NOT NULL DEFAULT 1 CHECK(wakes IN (0,1)),
    about TEXT,
    about_id TEXT,
    kind TEXT
);
CREATE INDEX space_messages_space ON space_messages(space_id, id);
INSERT INTO notifications(id,created_at,workspace,space_id,subject,kind,message,read)
    SELECT id,created_at,space,space_id,subject,
        CASE kind WHEN 'message' THEN 'agent_message' ELSE kind END,message,read
    FROM alerts WHERE recipient_id IS NULL;
INSERT INTO space_messages(id,space_id,created_at,message,attachments,wakes,about,about_id,kind)
    SELECT id,recipient_id,created_at,message,attachments,wakes,space,space_id,
        CASE WHEN for_user=0 THEN NULL WHEN kind='message' THEN 'agent_message' ELSE kind END
    FROM alerts WHERE recipient_id IS NOT NULL AND read=0;
DROP TABLE alerts;
DELETE FROM sqlite_sequence WHERE name='alerts';
DROP TRIGGER space_removed;
CREATE TRIGGER space_removed AFTER DELETE ON spaces BEGIN
    DELETE FROM space_parents WHERE space_id=OLD.id OR parent_id=OLD.id;
    DELETE FROM space_issue WHERE space_id=OLD.id;
    DELETE FROM pr_cleanup WHERE space_id=OLD.id;
    DELETE FROM space_items WHERE space_id=OLD.id;
    DELETE FROM pr_activity WHERE space_id=OLD.id;
    DELETE FROM space_item_activity WHERE space_id=OLD.id;
    DELETE FROM space_review WHERE space_id=OLD.id;
    DELETE FROM pending_updates WHERE space_id=OLD.id;
    DELETE FROM cleanup_grace WHERE space_id=OLD.id;
    DELETE FROM space_agent_state WHERE space_id=OLD.id;
    DELETE FROM space_progress WHERE space_id=OLD.id;
    DELETE FROM space_conversions WHERE space_id=OLD.id;
    DELETE FROM space_completion WHERE space_id=OLD.id;
    DELETE FROM space_turns WHERE space_id=OLD.id;
    DELETE FROM space_priority WHERE space_id=OLD.id;
    DELETE FROM space_sleep WHERE space_id=OLD.id;
    DELETE FROM space_messages WHERE space_id=OLD.id;
    DELETE FROM space_pr_titles WHERE space_id=OLD.id;
    DELETE FROM space_turn_retries WHERE space_id=OLD.id;
    DELETE FROM gate_sessions WHERE space_id=OLD.id;
    DELETE FROM gate_verdicts WHERE space_id=OLD.id;
    DELETE FROM executions WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE scope_space_id=OLD.id AND space_id IS NULL;
END;
