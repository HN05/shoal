-- Every space's parent, a workspace or a chat in the same repository, whose
-- agent works on it or coordinates its agent. A space without a row is
-- top-level. Writers check the parent; removal takes a space's children
-- first. It replaces the workspace a workspace was split from and the chat
-- that opened it: the workspace it was split from wins, as routing chose it.
-- A chat in another repository, which could open workspaces before, gives
-- its workspace back to the user, since a child is in its parent's
-- repository.
CREATE TABLE space_parents (
    space_id TEXT PRIMARY KEY,
    parent_id TEXT NOT NULL
);
CREATE INDEX space_parents_parent ON space_parents(parent_id);
INSERT INTO space_parents(space_id, parent_id)
    SELECT id, parent FROM (SELECT id, COALESCE(
            (SELECT parent.id FROM workspaces AS parent
                WHERE parent.id=workspaces.parent_workspace_id
                    AND parent.repository_id=workspaces.repository_id),
            (SELECT chats.id FROM chats WHERE chats.id=workspaces.chat_id
                AND chats.repository_id=workspaces.repository_id)) AS parent
        FROM workspaces)
    WHERE parent IS NOT NULL;

DROP TRIGGER workspace_chat_changed;
DROP TRIGGER workspace_created;
DROP INDEX workspaces_chat;
DROP INDEX workspaces_parent;
ALTER TABLE workspaces DROP COLUMN chat_id;
ALTER TABLE workspaces DROP COLUMN parent_workspace_id;

-- Writers record a new workspace's parent before the workspace itself, so
-- its creation names it.
CREATE TRIGGER workspace_created AFTER INSERT ON workspaces BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','created','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL,
        'base_workspace',json((SELECT json_object('id',id,'name',name,'branch',branch)
            FROM workspaces WHERE id=NEW.base_workspace_id)),
        'parent',json((SELECT json_object('id',id,'name',name,'kind','workspace','branch',branch)
                FROM workspaces WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id)
            UNION ALL SELECT json_object('id',id,'name',name,'kind','chat')
                FROM chats WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id)))
    ));
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','ready','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL
    ) WHERE NEW.state='ready';
END;

-- A workspace that exists already changes parent when a chat opens it or it
-- returns to the user.
CREATE TRIGGER space_parent_set AFTER INSERT ON space_parents BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','parent_changed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'parent',json((SELECT json_object('id',id,'name',name,'kind','workspace','branch',branch)
                FROM workspaces WHERE id=NEW.parent_id
            UNION ALL SELECT json_object('id',id,'name',name,'kind','chat')
                FROM chats WHERE id=NEW.parent_id))
    ) FROM workspaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_parent_changed AFTER UPDATE OF parent_id ON space_parents
WHEN NEW.parent_id IS NOT OLD.parent_id BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','parent_changed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'parent',json((SELECT json_object('id',id,'name',name,'kind','workspace','branch',branch)
                FROM workspaces WHERE id=NEW.parent_id
            UNION ALL SELECT json_object('id',id,'name',name,'kind','chat')
                FROM chats WHERE id=NEW.parent_id))
    ) FROM workspaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_parent_cleared AFTER DELETE ON space_parents BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','parent_changed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,'parent',NULL
    ) FROM workspaces WHERE id=OLD.space_id;
END;

-- A removed space leaves the tree. Removal takes its children first; any
-- left become top-level, as a chat's did when it ended.
CREATE TRIGGER workspace_left_tree AFTER DELETE ON workspaces BEGIN
    DELETE FROM space_parents WHERE space_id=OLD.id OR parent_id=OLD.id;
END;
CREATE TRIGGER chat_left_tree AFTER DELETE ON chats BEGIN
    DELETE FROM space_parents WHERE space_id=OLD.id OR parent_id=OLD.id;
END;
