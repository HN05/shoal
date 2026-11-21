-- When each machine was last seen before the hub alerted the user that it
-- stayed unreachable while its spaces had something to do, so one drop
-- alerts once.
ALTER TABLE hub_members ADD COLUMN drop_alerted INTEGER;
