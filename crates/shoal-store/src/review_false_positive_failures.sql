-- A review also fails for its repository's invalid false-positive file.
-- Dropping the old table deletes the findings and replies that reference
-- it, so they are kept aside and restored, as is the last review ID, so IDs
-- are not reused.
CREATE TEMP TABLE kept_review_findings AS SELECT * FROM review_findings;
CREATE TEMP TABLE kept_review_replies AS SELECT * FROM review_replies;
CREATE TEMP TABLE kept_review_sequence AS SELECT seq FROM sqlite_sequence WHERE name='reviews';

CREATE TABLE reviews_failing (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    -- The JSON target: the workspace, or a PR and whether to post there.
    target TEXT NOT NULL,
    agent TEXT NOT NULL,
    -- The JSON model and effort asked for the agent, when any.
    model TEXT,
    reviewer TEXT,
    state TEXT NOT NULL CHECK(state IN ('queued','running','finished','failed','cancelled')),
    failure TEXT CHECK(failure IN ('usage-limit','quota-exhausted','rate-limit',
        'authentication-failed','execution-failed','invalid-output','timed-out',
        'invalid-false-positives')),
    head TEXT,
    base TEXT,
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    ended_at INTEGER,
    summary TEXT,
    verdict TEXT CHECK(verdict IN ('approve','comment','request_changes')),
    -- The JSON result of posting a PR review.
    posted TEXT,
    -- The workspace whose process cancelled the review, whose agents are
    -- not told about it.
    cancelled_by TEXT,
    -- Whether protection stopped its run, so it waits for healthy readings
    -- before it runs again, through a daemon restart too.
    waits_for_recovery INTEGER NOT NULL DEFAULT 0,
    CHECK((state='failed') = (failure IS NOT NULL))
);
INSERT INTO reviews_failing(id,workspace_id,target,agent,model,reviewer,state,failure,head,
        base,created_at,started_at,ended_at,summary,verdict,posted,cancelled_by,
        waits_for_recovery)
    SELECT id,workspace_id,target,agent,model,reviewer,state,failure,head,base,created_at,
        started_at,ended_at,summary,verdict,posted,cancelled_by,waits_for_recovery
    FROM reviews ORDER BY id;
DROP TABLE reviews;
ALTER TABLE reviews_failing RENAME TO reviews;
CREATE INDEX reviews_workspace ON reviews(workspace_id, id);
CREATE UNIQUE INDEX reviews_active ON reviews(workspace_id)
    WHERE state IN ('queued','running');
DELETE FROM sqlite_sequence WHERE name='reviews';
INSERT INTO sqlite_sequence(name,seq) SELECT 'reviews',seq FROM kept_review_sequence;

INSERT INTO review_findings SELECT * FROM kept_review_findings;
INSERT INTO review_replies SELECT * FROM kept_review_replies;
DROP TABLE kept_review_findings;
DROP TABLE kept_review_replies;
DROP TABLE kept_review_sequence;
