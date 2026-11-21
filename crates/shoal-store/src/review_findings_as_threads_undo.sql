-- What tests that emulate an earlier database run first to undo
-- review_findings_as_threads.sql: a review's findings are its own rows.
DROP TABLE review_reopens;
CREATE TABLE review_findings (
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
CREATE TABLE review_replies (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    review_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    body TEXT NOT NULL,
    FOREIGN KEY(review_id, number) REFERENCES review_findings(review_id, number) ON DELETE CASCADE
);
CREATE INDEX review_replies_review ON review_replies(review_id, id);
