-- What tests that emulate an earlier database run first to undo
-- space_events.sql: the journal knows no spaces again, and only workspaces
-- record into it.
DROP TRIGGER chat_completed; DROP TRIGGER chat_undone;
DROP TRIGGER space_review_ready; DROP TRIGGER space_review_ready_again;
DROP TRIGGER space_review_cleared; DROP TRIGGER space_agent_state_set;
DROP TRIGGER space_agent_state_changed; DROP TRIGGER space_agent_state_cleared;
DROP TRIGGER workspace_renamed; DROP TRIGGER question_asked;
DROP TRIGGER question_answered; DROP TRIGGER workspace_child_created;
DROP TRIGGER chat_child_created; DROP TRIGGER workspace_base_recorded;
DROP TRIGGER workspace_name_changed;
DROP TRIGGER workspace_left_tree;
DROP TRIGGER chat_left_tree;
CREATE TRIGGER workspace_left_tree AFTER DELETE ON workspaces BEGIN
    DELETE FROM space_parents WHERE space_id=OLD.id OR parent_id=OLD.id;
END;
CREATE TRIGGER chat_left_tree AFTER DELETE ON chats BEGIN
    DELETE FROM space_parents WHERE space_id=OLD.id OR parent_id=OLD.id;
END;
-- The ready marks and turn states of workspaces alone reach it again.
CREATE TRIGGER space_review_ready AFTER INSERT ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''),'head',NEW.head)
    ) FROM workspaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_review_ready_again AFTER UPDATE ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''),'head',NEW.head)
    ) FROM workspaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_review_cleared AFTER DELETE ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_cleared','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'review',json_object('kind',OLD.kind,'url',nullif(OLD.url,''),'head',OLD.head)
    ) FROM workspaces WHERE id=OLD.space_id;
END;
CREATE TRIGGER space_agent_state_set AFTER INSERT ON space_agent_state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'agent_state',json_object('state',NEW.state)
    ) FROM workspaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_agent_state_changed AFTER UPDATE OF state ON space_agent_state
WHEN OLD.state IS NOT NEW.state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'agent_state',json_object('state',NEW.state)
    ) FROM workspaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_agent_state_cleared AFTER DELETE ON space_agent_state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'agent_state',json_object('state',NULL)
    ) FROM workspaces WHERE id=OLD.space_id;
END;
DROP VIEW journal_spaces;
DROP INDEX workspace_events_space; DROP TRIGGER workspace_events_retention;
ALTER TABLE workspace_events DROP COLUMN bulk; DROP TABLE journal_pruned;
ALTER TABLE workspace_events DROP COLUMN space_id;
CREATE TRIGGER workspace_events_retention AFTER INSERT ON workspace_events BEGIN
    DELETE FROM workspace_events WHERE id <= (
        SELECT id FROM workspace_events ORDER BY id DESC LIMIT 1 OFFSET 1000
    );
END;
