-- What tests that emulate an earlier database run first to undo
-- marks_without_heads.sql: marks record a commit again.
DROP TRIGGER space_review_ready;
DROP TRIGGER space_review_ready_again;
DROP TRIGGER space_review_cleared;
ALTER TABLE space_review ADD COLUMN head TEXT NOT NULL DEFAULT '';
CREATE TRIGGER space_review_ready AFTER INSERT ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''),'head',NEW.head)
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_review_ready_again AFTER UPDATE ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''),'head',NEW.head)
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_review_cleared AFTER DELETE ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_cleared','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'review',json_object('kind',OLD.kind,'url',nullif(OLD.url,''),'head',OLD.head)
    ) FROM journal_spaces WHERE id=OLD.space_id;
END;
