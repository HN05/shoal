-- A move keeps its destination's kind in a column, which queries read,
-- rather than in the destination's JSON. The migration moves it from the
-- moves recorded before, and the next one makes the column required.
ALTER TABLE space_moves ADD COLUMN destination_kind TEXT;
