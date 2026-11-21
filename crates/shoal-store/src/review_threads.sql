-- A `review` thread is a local review's finding: `severity` says how much
-- it matters, and `review_id` names the review that opened it.
ALTER TABLE threads ADD COLUMN severity TEXT CHECK(severity IN ('blocker','should-fix','nit'));
ALTER TABLE threads ADD COLUMN review_id INTEGER;
CREATE INDEX threads_review ON threads(review_id) WHERE review_id IS NOT NULL;
