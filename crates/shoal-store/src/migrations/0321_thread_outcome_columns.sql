-- A resolved thread keeps its outcome's kind in `outcome` and the cause of
-- a closing in `cause`, which queries read, and what else the outcome says
-- in `outcome_details`. Migration 322 rewrites the outcomes into them.
ALTER TABLE threads ADD COLUMN cause TEXT;
ALTER TABLE threads ADD COLUMN outcome_details TEXT;
-- The rewrite changes each outcome, which this trigger would journal as a
-- resolution; migration 322 makes it anew.
DROP TRIGGER thread_resolved;
