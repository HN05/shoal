-- The journal records a resolution's outcome by its kind, which is all the
-- column holds now. A resolution that changes only the outcome's cause or
-- details, such as the issue a deferral names, is journaled too.
CREATE TRIGGER thread_resolved AFTER UPDATE OF outcome,cause,outcome_details ON threads
WHEN (OLD.outcome IS NOT NEW.outcome OR OLD.cause IS NOT NEW.cause
        OR OLD.outcome_details IS NOT NEW.outcome_details)
    AND NOT NEW.held BEGIN
    SELECT journal_append('thread_resolved', NEW.space_id,
        json_object('space_number', NEW.space_number, 'number', NEW.number, 'name', NEW.name,
            'outcome', NEW.outcome));
END;
