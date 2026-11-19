-- A space's assigned issue records who linked it, the user's terminal or a
-- scoped caller, whose agent then works it, and the user's own login it
-- stands for among the issue's assignees. Shoal reads the forge for the
-- agent account instead of recording it, so its records go.
ALTER TABLE space_issue RENAME COLUMN assigner TO linked_by;
UPDATE space_issue SET assignee=NULL WHERE linked_by='agent';
UPDATE space_issue SET linked_by='user' WHERE linked_by IS NULL;
DELETE FROM issue_assignments WHERE account='agent';
ALTER TABLE issue_assignments DROP COLUMN account;
