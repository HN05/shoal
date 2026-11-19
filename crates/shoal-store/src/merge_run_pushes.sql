-- A space's lookup of the runs of a merge also follows a merge `shoal
-- merge` pushed, which no PR made: each lookup is the space's for its merge
-- commit, and names either the PR that merged or the URL of the remote the
-- merge was pushed to, whose forge lists its runs.
CREATE TABLE merge_runs_keyed (
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    pr_url TEXT,
    remote_url TEXT CHECK((pr_url IS NULL) <> (remote_url IS NULL)),
    merge_commit TEXT NOT NULL,
    branch TEXT NOT NULL,
    merged_at INTEGER NOT NULL,
    looking INTEGER NOT NULL DEFAULT 1 CHECK(looking IN (0,1)),
    runs TEXT NOT NULL DEFAULT '[]',
    ended INTEGER NOT NULL DEFAULT 0 CHECK(ended IN (0,1)),
    PRIMARY KEY(space_id,merge_commit)
);
INSERT OR IGNORE INTO merge_runs_keyed(space_id,pr_url,merge_commit,branch,merged_at,looking,runs,ended)
    SELECT space_id,pr_url,merge_commit,branch,merged_at,looking,runs,ended FROM merge_runs
    ORDER BY merged_at;
DROP TABLE merge_runs;
ALTER TABLE merge_runs_keyed RENAME TO merge_runs;
