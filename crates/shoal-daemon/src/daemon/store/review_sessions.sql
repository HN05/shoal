-- The session each review's reviewer reported, which the next review of the
-- same workspace or PR resumes when the same agent reviews it.
ALTER TABLE reviews ADD COLUMN session TEXT;
