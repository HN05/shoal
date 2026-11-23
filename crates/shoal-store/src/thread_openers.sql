-- The space that opened a thread a space opened: the one it is numbered
-- in, or a space of another machine whose review of the workspace it is
-- numbered in it comments, with the number and name the hub listed for it.
ALTER TABLE threads ADD COLUMN opener_id TEXT;
ALTER TABLE threads ADD COLUMN opener_name TEXT;
ALTER TABLE threads ADD COLUMN opener_number INTEGER;
UPDATE threads SET opener_id=space_id WHERE opener='space';
