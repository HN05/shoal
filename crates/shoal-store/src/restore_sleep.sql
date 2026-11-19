-- Why Shoal put the agent of a restore it starts on its own to sleep, and
-- what wakes it, which lists show; a waiting wrapper's restore reads both
-- from the agent's recorded stop.
ALTER TABLE agent_queue ADD COLUMN stop_reason TEXT;
ALTER TABLE agent_queue ADD COLUMN wakes_when TEXT;
