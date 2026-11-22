-- A review names who writes it: Shoal, whose reviewer is a one-shot run of
-- an agent, or the user or a space's agent, who write it themselves while
-- it is pending, so only Shoal's reviews name an agent. Dropping the old
-- table deletes the reopens that reference it, so they are kept aside and
-- restored, as is the last review ID, so IDs are not reused.
CREATE TEMP TABLE kept_review_reopens AS SELECT * FROM review_reopens;
CREATE TEMP TABLE kept_review_sequence AS SELECT seq FROM sqlite_sequence WHERE name='reviews';

CREATE TABLE reviews_authored (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    -- The JSON target: the workspace, or a PR and whether to post there.
    target TEXT NOT NULL,
    -- Who writes it, and the space that does.
    opener TEXT NOT NULL CHECK(opener IN ('shoal','user','space')),
    opener_id TEXT,
    -- The agent Shoal's reviewer runs.
    agent TEXT,
    -- The JSON model and effort asked for the agent, when any.
    model TEXT,
    reviewer TEXT,
    state TEXT NOT NULL
        CHECK(state IN ('queued','running','pending','finished','failed','cancelled')),
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
    session TEXT,
    CHECK((state='failed') = (failure IS NOT NULL)),
    CHECK((opener='shoal') = (agent IS NOT NULL)),
    CHECK((opener='space') = (opener_id IS NOT NULL)),
    CHECK(opener='shoal' OR state IN ('pending','finished','cancelled'))
);
INSERT INTO reviews_authored(id,workspace_id,target,opener,agent,model,reviewer,state,failure,
        head,base,created_at,started_at,ended_at,summary,verdict,posted,cancelled_by,
        waits_for_recovery,session)
    SELECT id,workspace_id,target,'shoal',agent,model,reviewer,state,failure,head,base,
        created_at,started_at,ended_at,summary,verdict,posted,cancelled_by,waits_for_recovery,
        session
    FROM reviews ORDER BY id;
DROP TABLE reviews;
ALTER TABLE reviews_authored RENAME TO reviews;
CREATE INDEX reviews_workspace ON reviews(workspace_id, id);
CREATE UNIQUE INDEX reviews_active ON reviews(workspace_id)
    WHERE state IN ('queued','running');
-- The user and each space write one pending review of a workspace at a time.
CREATE UNIQUE INDEX reviews_pending ON reviews(workspace_id, opener, COALESCE(opener_id, ''))
    WHERE state='pending';
DELETE FROM sqlite_sequence WHERE name='reviews';
INSERT INTO sqlite_sequence(name,seq) SELECT 'reviews',seq FROM kept_review_sequence;

INSERT INTO review_reopens SELECT * FROM kept_review_reopens;
DROP TABLE kept_review_reopens;
DROP TABLE kept_review_sequence;
