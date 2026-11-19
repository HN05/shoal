-- What tests that emulate an earlier database run first to undo
-- space_records.sql: each kind's rows delete what the tables of spaces
-- hold for them.
DROP TRIGGER space_removed;
DROP TRIGGER workspace_recorded;
DROP TRIGGER chat_recorded;
DROP TRIGGER workspace_removed;
DROP TRIGGER chat_removed;
DROP TABLE space_records;
CREATE TRIGGER workspace_links_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_issue WHERE space_id=old.id;
    DELETE FROM pr_cleanup WHERE space_id=old.id;
    DELETE FROM space_items WHERE space_id=old.id;
    DELETE FROM pr_activity WHERE space_id=old.id;
    DELETE FROM space_item_activity WHERE space_id=old.id;
END;
CREATE TRIGGER chat_links_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_issue WHERE space_id=old.id;
    DELETE FROM pr_cleanup WHERE space_id=old.id;
    DELETE FROM space_items WHERE space_id=old.id;
    DELETE FROM pr_activity WHERE space_id=old.id;
    DELETE FROM space_item_activity WHERE space_id=old.id;
END;
CREATE TRIGGER workspace_review_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_review WHERE space_id=old.id;
END;
CREATE TRIGGER chat_review_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_review WHERE space_id=old.id;
END;
CREATE TRIGGER workspace_pending_updates_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM pending_updates WHERE space_id=old.id;
END;
CREATE TRIGGER chat_pending_updates_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM pending_updates WHERE space_id=old.id;
END;
CREATE TRIGGER workspace_grace_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM cleanup_grace WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_grace_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM cleanup_grace WHERE space_id=OLD.id;
END;
CREATE TRIGGER workspace_agent_state_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_agent_state WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_agent_state_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_agent_state WHERE space_id=OLD.id;
END;
CREATE TRIGGER workspace_progress_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_progress WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_progress_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_progress WHERE space_id=OLD.id;
END;
CREATE TRIGGER workspace_conversion_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_conversions WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_conversion_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_conversions WHERE space_id=OLD.id;
END;
CREATE TRIGGER workspace_completion_removed AFTER DELETE ON workspaces BEGIN
    DELETE FROM space_completion WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_completion_removed AFTER DELETE ON chats BEGIN
    DELETE FROM space_completion WHERE space_id=OLD.id;
END;
CREATE TRIGGER workspace_turns_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_turns WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_turns_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_turns WHERE space_id=OLD.id;
END;
CREATE TRIGGER workspace_priority_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_priority WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_priority_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_priority WHERE space_id=OLD.id;
END;
CREATE TRIGGER workspace_sleep_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_sleep WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_sleep_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_sleep WHERE space_id=OLD.id;
END;
CREATE TRIGGER workspace_executions_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM executions WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_executions_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM executions WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE space_id=OLD.id;
END;
CREATE TRIGGER workspace_queued_launches_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM agent_queue WHERE scope_space_id=OLD.id AND space_id IS NULL;
END;
CREATE TRIGGER chat_queued_launches_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM agent_queue WHERE scope_space_id=OLD.id AND space_id IS NULL;
END;
CREATE TRIGGER workspace_left_tree AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_removed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'child',json_object('id',OLD.id,'name',OLD.name,'kind','workspace','branch',OLD.branch)
    ) FROM journal_spaces
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=OLD.id);
    DELETE FROM space_parents WHERE space_id=OLD.id OR parent_id=OLD.id;
END;
CREATE TRIGGER chat_left_tree AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_removed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'child',json_object('id',OLD.id,'name',OLD.name,'kind','chat')
    ) FROM journal_spaces
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=OLD.id);
    DELETE FROM space_parents WHERE space_id=OLD.id OR parent_id=OLD.id;
END;
