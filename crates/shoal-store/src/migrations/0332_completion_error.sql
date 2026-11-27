-- A completion keeps its cleanup error in a column, which the trigger that
-- journals a repeated completion tests, rather than in its JSON. The
-- migration moves it from the completions stored before, with the trigger
-- gone until the next migration, so that the move journals nothing.
ALTER TABLE space_completion ADD COLUMN error TEXT;
DROP TRIGGER space_completed_again;
