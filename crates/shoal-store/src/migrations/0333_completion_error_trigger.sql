CREATE TRIGGER space_completed_again AFTER UPDATE OF record ON space_completion
WHEN NEW.error IS NULL BEGIN
    SELECT journal_append('completed', NEW.space_id, json_object('cause', NEW.cause));
END;
