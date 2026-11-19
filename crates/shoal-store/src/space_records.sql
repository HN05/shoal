-- Every space has one record, whatever its kind, which its row in
-- `workspaces` or `chats` extends. A conversion replaces that row and keeps
-- the record: the new row goes in first, then the old one goes. Deleting the
-- record removes the space, with its row and what the tables of spaces hold
-- for it.
CREATE TABLE space_records (id TEXT PRIMARY KEY);
INSERT INTO space_records(id) SELECT id FROM workspaces UNION SELECT id FROM chats;

CREATE TRIGGER workspace_recorded AFTER INSERT ON workspaces BEGIN
    INSERT OR IGNORE INTO space_records(id) VALUES (NEW.id);
END;
CREATE TRIGGER chat_recorded AFTER INSERT ON chats BEGIN
    INSERT OR IGNORE INTO space_records(id) VALUES (NEW.id);
END;
-- The space's row goes first, so its kind's triggers journal its leaving
-- the tree while its parent is still recorded.
CREATE TRIGGER space_removed AFTER DELETE ON space_records BEGIN
    DELETE FROM workspaces WHERE id=OLD.id;
    DELETE FROM chats WHERE id=OLD.id;
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
    DELETE FROM executions WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE scope_space_id=OLD.id AND space_id IS NULL;
END;

DROP TRIGGER workspace_links_removed;
DROP TRIGGER chat_links_removed;
DROP TRIGGER workspace_review_removed;
DROP TRIGGER chat_review_removed;
DROP TRIGGER workspace_pending_updates_removed;
DROP TRIGGER chat_pending_updates_removed;
DROP TRIGGER workspace_grace_removed;
DROP TRIGGER chat_grace_removed;
DROP TRIGGER workspace_agent_state_removed;
DROP TRIGGER chat_agent_state_removed;
DROP TRIGGER workspace_progress_removed;
DROP TRIGGER chat_progress_removed;
DROP TRIGGER workspace_conversion_removed;
DROP TRIGGER chat_conversion_removed;
DROP TRIGGER workspace_completion_removed;
DROP TRIGGER chat_completion_removed;
DROP TRIGGER workspace_turns_removed;
DROP TRIGGER chat_turns_removed;
DROP TRIGGER workspace_priority_removed;
DROP TRIGGER chat_priority_removed;
DROP TRIGGER workspace_sleep_removed;
DROP TRIGGER chat_sleep_removed;
DROP TRIGGER workspace_executions_removed;
DROP TRIGGER chat_executions_removed;
DROP TRIGGER workspace_queued_launches_removed;
DROP TRIGGER chat_queued_launches_removed;

-- A space's row whose removal no other kind's row replaces removes the
-- space: its leaving the tree is journaled for its kind, while its parent
-- is still recorded, then its record goes.
DROP TRIGGER workspace_left_tree;
CREATE TRIGGER workspace_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_removed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'child',json_object('id',OLD.id,'name',OLD.name,'kind','workspace','branch',OLD.branch)
    ) FROM journal_spaces
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=OLD.id);
    DELETE FROM space_records WHERE id=OLD.id;
END;
DROP TRIGGER chat_left_tree;
CREATE TRIGGER chat_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_removed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'child',json_object('id',OLD.id,'name',OLD.name,'kind','chat')
    ) FROM journal_spaces
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=OLD.id);
    DELETE FROM space_records WHERE id=OLD.id;
END;
