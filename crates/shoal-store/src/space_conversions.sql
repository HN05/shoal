-- A chat becomes a workspace and a workspace a chat by moving its row to the
-- other table under the same ID and number: the new row goes in first, then
-- the old one goes. What belongs to the space, such as its links, progress,
-- parent and children, stays while the space exists as the other kind, so
-- the old row's removal triggers leave it, and the new row's creation is
-- journaled as a conversion instead of a new space.
DROP TRIGGER workspace_links_removed;
CREATE TRIGGER workspace_links_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_issue WHERE space_id=old.id;
    DELETE FROM pr_cleanup WHERE space_id=old.id;
    DELETE FROM space_items WHERE space_id=old.id;
    DELETE FROM pr_activity WHERE space_id=old.id;
    DELETE FROM space_item_activity WHERE space_id=old.id;
END;
DROP TRIGGER chat_links_removed;
CREATE TRIGGER chat_links_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_issue WHERE space_id=old.id;
    DELETE FROM pr_cleanup WHERE space_id=old.id;
    DELETE FROM space_items WHERE space_id=old.id;
    DELETE FROM pr_activity WHERE space_id=old.id;
    DELETE FROM space_item_activity WHERE space_id=old.id;
END;
DROP TRIGGER workspace_review_removed;
CREATE TRIGGER workspace_review_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_review WHERE space_id=old.id;
END;
DROP TRIGGER chat_review_removed;
CREATE TRIGGER chat_review_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_review WHERE space_id=old.id;
END;
DROP TRIGGER workspace_pending_updates_removed;
CREATE TRIGGER workspace_pending_updates_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM pending_updates WHERE space_id=old.id;
END;
DROP TRIGGER chat_pending_updates_removed;
CREATE TRIGGER chat_pending_updates_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM pending_updates WHERE space_id=old.id;
END;
DROP TRIGGER workspace_grace_removed;
CREATE TRIGGER workspace_grace_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM cleanup_grace WHERE space_id=OLD.id;
END;
DROP TRIGGER chat_grace_removed;
CREATE TRIGGER chat_grace_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM cleanup_grace WHERE space_id=OLD.id;
END;
DROP TRIGGER workspace_agent_state_removed;
CREATE TRIGGER workspace_agent_state_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_agent_state WHERE space_id=OLD.id;
END;
DROP TRIGGER chat_agent_state_removed;
CREATE TRIGGER chat_agent_state_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_agent_state WHERE space_id=OLD.id;
END;
DROP TRIGGER workspace_progress_removed;
CREATE TRIGGER workspace_progress_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_progress WHERE space_id=OLD.id;
END;
DROP TRIGGER chat_progress_removed;
CREATE TRIGGER chat_progress_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_progress WHERE space_id=OLD.id;
END;
DROP TRIGGER workspace_left_tree;
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
DROP TRIGGER chat_left_tree;
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
DROP TRIGGER chat_ended;
CREATE TRIGGER chat_ended AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_ended','chat',json_object('id',OLD.id,'number',OLD.number,
            'repository_id',OLD.repository_id,'name',OLD.name,'agent',OLD.agent,'pid',OLD.pid,
            'started_at',OLD.started_at)
    ));
END;

DROP TRIGGER workspace_created;
CREATE TRIGGER workspace_created AFTER INSERT ON workspaces
WHEN NEW.id NOT IN (SELECT id FROM chats) BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','created','workspace_id',NEW.id,'chat_id',NULL,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL,
        'base_ref',NEW.base_ref,
        'base_workspace',json((SELECT json_object('id',id,'name',name,'branch',branch)
            FROM workspaces WHERE id=NEW.base_workspace_id)),
        'parent',json((SELECT json_object('id',id,'name',name,'kind','workspace','branch',branch)
                FROM workspaces WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id)
            UNION ALL SELECT json_object('id',id,'name',name,'kind','chat')
                FROM chats WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id)))
    ));
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','ready','workspace_id',NEW.id,'chat_id',NULL,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL
    ) WHERE NEW.state='ready';
END;
DROP TRIGGER workspace_child_created;
CREATE TRIGGER workspace_child_created AFTER INSERT ON workspaces
WHEN NEW.id NOT IN (SELECT id FROM chats) BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_created','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'child',json_object('id',NEW.id,'name',NEW.name,'kind','workspace','branch',NEW.branch)
    ) FROM journal_spaces
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id);
END;
DROP TRIGGER chat_started;
CREATE TRIGGER chat_started AFTER INSERT ON chats
WHEN NEW.id NOT IN (SELECT id FROM workspaces) BEGIN
    UPDATE chats SET number=(SELECT number FROM free_space_number)
        WHERE id=NEW.id AND number IS NULL;
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','chat_started','chat',json_object('id',id,'number',number,
            'repository_id',repository_id,'name',name,'agent',agent,'pid',pid,
            'started_at',started_at)
    ) FROM chats WHERE id=NEW.id;
END;
CREATE TRIGGER workspace_converted AFTER INSERT ON workspaces
WHEN NEW.id IN (SELECT id FROM chats) BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','converted','workspace_id',NEW.id,'chat_id',NULL,
        'repository_id',NEW.repository_id,'name',NEW.name,'path',NEW.path,
        'branch',NEW.branch,'cause',NULL,'error',NULL
    ));
END;
CREATE TRIGGER chat_converted AFTER INSERT ON chats
WHEN NEW.id IN (SELECT id FROM workspaces) BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','converted','workspace_id',NULL,'chat_id',NEW.id,
        'repository_id',NEW.repository_id,'name',NEW.name,'path',NULL,
        'branch',NULL,'cause',NULL,'error',NULL
    ));
END;

-- A conversion whose agent has yet to start in the new place: until its
-- turn ends, the agent runs where the space was, and `record` holds what
-- restarting it takes, and for a chat the row it had, which a workspace
-- whose worktree could not be made gives back.
CREATE TABLE space_conversions (
    space_id TEXT PRIMARY KEY,
    record TEXT NOT NULL
);
CREATE TRIGGER workspace_conversion_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_conversions WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_conversion_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_conversions WHERE space_id=OLD.id;
END;

-- A stopped chat a workspace became, whose restored agent is told so.
ALTER TABLE chats ADD COLUMN converted INTEGER NOT NULL DEFAULT 0 CHECK(converted IN (0,1));
