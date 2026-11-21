-- A move that holds its package for the joined machine that started it
-- waits for that machine until `held_until`, which its packing and each
-- read of its package set, before Shoal finishes it as one whose reader
-- went.
ALTER TABLE space_moves ADD COLUMN held_until INTEGER;
