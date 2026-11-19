-- A launch that waits for its workspace's preparation and then starts at
-- once, regardless of the limits, as a start asked for it.
ALTER TABLE agent_queue ADD COLUMN starts_at_once INTEGER NOT NULL DEFAULT 0;
