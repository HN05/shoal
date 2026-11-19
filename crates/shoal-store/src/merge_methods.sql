-- A local merge keeps the method it merged by, which tells how to find
-- what it left the default branch at and how to revert it.
ALTER TABLE merges ADD COLUMN method TEXT NOT NULL DEFAULT 'merge'
    CHECK(method IN ('merge','rebase','squash'));
