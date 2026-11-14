-- What tests that emulate an earlier database run first to undo
-- issue_assignees.sql: Shoal records no forge assignees.
DROP TABLE issue_assignments;
ALTER TABLE space_issue DROP COLUMN assignee;
