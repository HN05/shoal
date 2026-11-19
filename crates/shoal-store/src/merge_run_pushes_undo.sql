-- What tests that emulate an earlier database run first to undo
-- merge_run_pushes.sql.
CREATE TABLE merge_runs_by_pr (
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    pr_url TEXT NOT NULL,
    merge_commit TEXT NOT NULL,
    branch TEXT NOT NULL,
    merged_at INTEGER NOT NULL,
    looking INTEGER NOT NULL DEFAULT 1 CHECK(looking IN (0,1)),
    runs TEXT NOT NULL DEFAULT '[]',
    ended INTEGER NOT NULL DEFAULT 0 CHECK(ended IN (0,1)),
    PRIMARY KEY(space_id,pr_url)
);
DROP TABLE merge_runs;
ALTER TABLE merge_runs_by_pr RENAME TO merge_runs;
