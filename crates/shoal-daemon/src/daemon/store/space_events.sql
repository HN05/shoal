-- The space each event belongs to, a workspace or a chat, which a space's
-- log reads by: the workspace or chat it records, or the chat a chat event
-- names.
ALTER TABLE workspace_events ADD COLUMN space_id TEXT GENERATED ALWAYS AS (COALESCE(
    json_extract(record,'$.workspace_id'), json_extract(record,'$.chat_id'),
    json_extract(record,'$.chat.id'))) VIRTUAL;
-- The events a space records often and its log leaves out: its agent's
-- turn states, and forge activity other than an item's closing, merging
-- and reopening.
ALTER TABLE workspace_events ADD COLUMN bulk INTEGER GENERATED ALWAYS AS (
    json_extract(record,'$.kind')='agent_state'
    OR (json_extract(record,'$.kind')='item_activity'
        AND json_extract(record,'$.item_activity.update.kind')
            NOT IN ('closed','merged','reopened'))) VIRTUAL;
CREATE INDEX workspace_events_space ON workspace_events(space_id, bulk, id);

-- Each space keeps its newest 500 bulk events, so a busy space cannot push
-- out another's history; the rest stay while the space does, and the
-- retention sweep forgets a removed space's events with its removal record.
-- The highest ID either deleted, so a replay from an older cursor reports
-- the gap.
CREATE TABLE journal_pruned (id INTEGER NOT NULL);
INSERT INTO journal_pruned(id) VALUES (0);
DROP TRIGGER workspace_events_retention;
CREATE TRIGGER workspace_events_retention AFTER INSERT ON workspace_events
WHEN NEW.bulk BEGIN
    UPDATE journal_pruned SET id=MAX(id, COALESCE((
        SELECT id FROM workspace_events WHERE space_id=NEW.space_id AND bulk
            ORDER BY id DESC LIMIT 1 OFFSET 500
    ), 0));
    DELETE FROM workspace_events WHERE space_id=NEW.space_id AND bulk AND id <= (
        SELECT id FROM workspace_events WHERE space_id=NEW.space_id AND bulk
            ORDER BY id DESC LIMIT 1 OFFSET 500
    );
END;

-- What an event of a space records about it: a workspace, or a chat, which
-- has no path or branch.
CREATE VIEW journal_spaces AS
    SELECT id, id AS workspace_id, NULL AS chat_id, repository_id, name, path, branch
        FROM workspaces
    UNION ALL SELECT id, NULL, id, repository_id, name, NULL, NULL FROM chats;

-- A chat's ready marks, turn states and completion reach the journal as a
-- workspace's do.
DROP TRIGGER space_review_ready;
CREATE TRIGGER space_review_ready AFTER INSERT ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''),'head',NEW.head)
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
DROP TRIGGER space_review_ready_again;
CREATE TRIGGER space_review_ready_again AFTER UPDATE ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''),'head',NEW.head)
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
-- Removal deletes a space's marks after its row is gone, so only explicit
-- withdrawal and unlinking record an event.
DROP TRIGGER space_review_cleared;
CREATE TRIGGER space_review_cleared AFTER DELETE ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_cleared','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'review',json_object('kind',OLD.kind,'url',nullif(OLD.url,''),'head',OLD.head)
    ) FROM journal_spaces WHERE id=OLD.space_id;
END;

DROP TRIGGER space_agent_state_set;
CREATE TRIGGER space_agent_state_set AFTER INSERT ON space_agent_state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,'agent_state',json_object('state',NEW.state)
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
-- Repeating a state is not a change.
DROP TRIGGER space_agent_state_changed;
CREATE TRIGGER space_agent_state_changed AFTER UPDATE OF state ON space_agent_state
WHEN OLD.state IS NOT NEW.state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,'agent_state',json_object('state',NEW.state)
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
-- Removal deletes a space's state after its row is gone, so only the end of
-- the reporting agent records a cleared state.
DROP TRIGGER space_agent_state_cleared;
CREATE TRIGGER space_agent_state_cleared AFTER DELETE ON space_agent_state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,'agent_state',json_object('state',NULL)
    ) FROM journal_spaces WHERE id=OLD.space_id;
END;

-- `shoal done` in a chat, and its withdrawal; a repeated `done` records its
-- completion again, as a workspace's does.
CREATE TRIGGER chat_completed AFTER UPDATE OF completion ON chats
WHEN NEW.completion IS NOT NULL BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','completed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause','manual','error',NULL
    ) FROM journal_spaces WHERE id=NEW.id;
END;
CREATE TRIGGER chat_undone AFTER UPDATE OF completion ON chats
WHEN OLD.completion IS NOT NULL AND NEW.completion IS NULL BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','undone','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL
    ) FROM journal_spaces WHERE id=NEW.id;
END;

-- A workspace's title; a chat's new name is its own `chat_renamed`.
CREATE TRIGGER workspace_renamed AFTER UPDATE OF title ON workspaces
WHEN NEW.title IS NOT OLD.title BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','renamed','workspace_id',NEW.id,'chat_id',NULL,
        'repository_id',NEW.repository_id,'name',NEW.name,'path',NEW.path,
        'branch',NEW.branch,'cause',NULL,'error',NULL,'title',NEW.title
    ));
END;

-- A space's questions, as asked and once answered.
CREATE TRIGGER question_asked AFTER INSERT ON questions BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','question_asked','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,'answer',NULL)
    ) FROM journal_spaces WHERE id=COALESCE(NEW.workspace_id,NEW.chat_id);
END;
CREATE TRIGGER question_answered AFTER UPDATE OF answer ON questions
WHEN OLD.answer IS NULL AND NEW.answer IS NOT NULL BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','question_answered','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,
            'answer',NEW.answer)
    ) FROM journal_spaces WHERE id=COALESCE(NEW.workspace_id,NEW.chat_id);
END;

-- A parent's log records the spaces created and removed below it. Writers
-- record a new workspace's parent before the workspace, and a new chat's
-- after it; a workspace a chat takes over existed already.
CREATE TRIGGER workspace_child_created AFTER INSERT ON workspaces BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_created','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'child',json_object('id',NEW.id,'name',NEW.name,'kind','workspace','branch',NEW.branch)
    ) FROM journal_spaces
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id);
END;
CREATE TRIGGER chat_child_created AFTER INSERT ON space_parents
WHEN NEW.space_id IN (SELECT id FROM chats) BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_created','workspace_id',parent.workspace_id,'chat_id',parent.chat_id,
        'repository_id',parent.repository_id,'name',parent.name,'path',parent.path,
        'branch',parent.branch,'cause',NULL,'error',NULL,
        'child',json_object('id',child.id,'name',child.name,'kind','chat')
    ) FROM journal_spaces AS parent, chats AS child
        WHERE parent.id=NEW.parent_id AND child.id=NEW.space_id;
END;
-- Before the removed space leaves the tree.
DROP TRIGGER workspace_left_tree;
CREATE TRIGGER workspace_left_tree AFTER DELETE ON workspaces BEGIN
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
CREATE TRIGGER chat_left_tree AFTER DELETE ON chats BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_removed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'child',json_object('id',OLD.id,'name',OLD.name,'kind','chat')
    ) FROM journal_spaces
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=OLD.id);
    DELETE FROM space_parents WHERE space_id=OLD.id OR parent_id=OLD.id;
END;

-- A workspace's creation names the ref it started from too.
DROP TRIGGER workspace_created;
CREATE TRIGGER workspace_created AFTER INSERT ON workspaces BEGIN
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
-- Creation records the base before Git resolves it into the worktree's,
-- so its event takes the ref once recorded.
CREATE TRIGGER workspace_base_recorded AFTER UPDATE OF base_ref ON workspaces
WHEN OLD.base_ref IS NULL AND NEW.base_ref IS NOT NULL BEGIN
    UPDATE workspace_events SET record=json_set(record,'$.base_ref',NEW.base_ref)
        WHERE id=(SELECT MAX(id) FROM workspace_events
            WHERE space_id=NEW.id AND json_extract(record,'$.kind')='created');
END;

-- A workspace's new name, which a branch rename gives it with its branch;
-- unlike a title change, it records no title.
CREATE TRIGGER workspace_name_changed AFTER UPDATE OF name ON workspaces
WHEN NEW.name IS NOT OLD.name BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','renamed','workspace_id',NEW.id,'chat_id',NULL,
        'repository_id',NEW.repository_id,'name',NEW.name,'path',NEW.path,
        'branch',NEW.branch,'cause',NULL,'error',NULL
    ));
END;
