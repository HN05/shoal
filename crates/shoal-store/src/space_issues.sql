-- A space may be assigned several issues, so each row is one of a space's
-- issues, and the order of rows is the order they were assigned in: a
-- space's issue so far is its first.
CREATE TABLE space_issue_copy AS SELECT space_id,url,error,title FROM space_issue;
DROP TABLE space_issue;
CREATE TABLE space_issue (
    space_id TEXT NOT NULL,
    url TEXT NOT NULL,
    error TEXT,
    title TEXT,
    PRIMARY KEY(space_id,url)
);
INSERT INTO space_issue(space_id,url,error,title)
    SELECT space_id,url,error,title FROM space_issue_copy;
DROP TABLE space_issue_copy;
