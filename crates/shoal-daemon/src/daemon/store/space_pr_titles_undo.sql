-- What tests that emulate an earlier database run first to undo
-- space_pr_titles.sql: assigned PRs record no titles.
DROP TRIGGER space_pr_titles_removed;
DROP TABLE space_pr_titles;
