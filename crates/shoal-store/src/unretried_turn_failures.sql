-- An unretried turn keeps the failure that ended it, which the space's
-- program status reports. Turns noted before then read as an API error.
ALTER TABLE execution_unretried_turns ADD COLUMN failure TEXT NOT NULL DEFAULT 'other';
