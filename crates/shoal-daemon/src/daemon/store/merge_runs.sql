-- A space's lookups of the default branch's CI runs for the merge commits
-- of the PRs it watched merge, where `[pr] watch_merge_runs` is on. Each
-- looks for runs of `merge_commit` on `branch` until a listing after ten
-- minutes from `merged_at`, when a watch first saw the merge, and `runs`
-- lists the runs it linked, which keep the space until they finish. It goes
-- with the space.
CREATE TABLE merge_runs (
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    pr_url TEXT NOT NULL,
    merge_commit TEXT NOT NULL,
    branch TEXT NOT NULL,
    merged_at INTEGER NOT NULL,
    looking INTEGER NOT NULL DEFAULT 1 CHECK(looking IN (0,1)),
    runs TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY(space_id,pr_url)
);
