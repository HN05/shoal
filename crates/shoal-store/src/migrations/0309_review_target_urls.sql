-- A review keeps the URL of the PR it covers in a column of its own, which
-- joins the reviews of one subject. The next migration's preparation fills
-- it from the JSON target.
ALTER TABLE reviews ADD COLUMN target_url TEXT;
