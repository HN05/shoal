-- Local reviews of a workspace's committed HEAD or a PR, their findings and
-- the replies to them; removing the workspace deletes them.
CREATE TABLE IF NOT EXISTS reviews (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    -- The JSON target: the workspace, or a PR and whether to post there.
    target TEXT NOT NULL,
    agent TEXT NOT NULL,
    reviewer TEXT,
    state TEXT NOT NULL CHECK(state IN ('queued','running','finished','failed','cancelled')),
    failure TEXT CHECK(failure IN ('usage-limit','quota-exhausted','rate-limit',
        'authentication-failed','execution-failed','invalid-output','timed-out')),
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
    CHECK((state='failed') = (failure IS NOT NULL))
);
-- At most one review of a workspace is queued or running.
CREATE UNIQUE INDEX IF NOT EXISTS reviews_active ON reviews(workspace_id)
    WHERE state IN ('queued','running');

CREATE TABLE IF NOT EXISTS review_findings (
    review_id INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    number INTEGER NOT NULL CHECK(number > 0),
    severity TEXT NOT NULL CHECK(severity IN ('blocker','should-fix','nit')),
    path TEXT NOT NULL,
    line INTEGER NOT NULL CHECK(line > 0),
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    -- The JSON outcome the workspace's agent resolved it with.
    outcome TEXT,
    check_result TEXT CHECK(check_result IN ('confirmed','still-present')),
    checked_by INTEGER,
    PRIMARY KEY(review_id, number),
    CHECK((check_result IS NULL) = (checked_by IS NULL))
);

CREATE TABLE IF NOT EXISTS review_replies (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    review_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    body TEXT NOT NULL,
    FOREIGN KEY(review_id, number) REFERENCES review_findings(review_id, number) ON DELETE CASCADE
);
