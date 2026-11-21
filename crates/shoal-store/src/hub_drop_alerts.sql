-- When each machine was last seen before the hub alerted the user that it
-- stayed unreachable without a word, so one drop alerts once.
ALTER TABLE hub_members ADD COLUMN drop_alerted INTEGER;
