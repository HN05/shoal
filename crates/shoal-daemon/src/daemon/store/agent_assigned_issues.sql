-- The issues this Shoal put or kept the agent account on while an agent of
-- its own worked them, which it alone takes the account off again: every
-- Shoal acting as the same account shares the forge assignee, so one whose
-- spaces do not work an issue leaves another's assignment alone. The
-- issues spaces link as assigned may have it from before.
CREATE TABLE agent_assigned_issues (url TEXT PRIMARY KEY);
INSERT OR IGNORE INTO agent_assigned_issues(url)
SELECT url FROM space_issue WHERE linked_by IS NOT NULL;
