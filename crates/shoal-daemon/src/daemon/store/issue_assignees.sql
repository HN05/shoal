-- A space's assigned issue records the login the forge shows assigned to it
-- for the space, whether Shoal assigned it or found it there, and whose
-- account that is; none where assigning is off or failed.
ALTER TABLE space_issue ADD COLUMN assignee TEXT;
ALTER TABLE space_issue ADD COLUMN assigner TEXT;
-- The logins Shoal added to an issue's assignees, and whose account it acted
-- as, so it takes back only its own once no space of that login links the
-- issue as assigned.
CREATE TABLE issue_assignments (
    url TEXT NOT NULL,
    login TEXT NOT NULL COLLATE NOCASE,
    account TEXT NOT NULL,
    PRIMARY KEY(url, login)
);
