-- A copy the hub kept of a space whose ID a claim took over is the one its
-- machine retires: it raises no conflict, and never takes the ID back once
-- the space ends on the machine that claimed it. The copies of a takeover
-- are such copies; a move's cannot be told from a duplicate.
ALTER TABLE hub_space_copies ADD COLUMN handed_over INTEGER NOT NULL DEFAULT 0;
UPDATE hub_space_copies SET handed_over=1 WHERE EXISTS(SELECT 1 FROM hub_takeovers t
    WHERE t.space_id=hub_space_copies.space_id AND t.member_id=hub_space_copies.member_id);
