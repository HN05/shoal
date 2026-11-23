-- A thread of a review the user or a space is writing holds its posts,
-- which reach nobody until the review is submitted.
ALTER TABLE threads ADD COLUMN held INTEGER NOT NULL DEFAULT 0 CHECK(held IN (0,1));
