-- The reset a turn retry waits past, when its upstream named one, through
-- the agent's proxy or a rate limit's error: while it waits, updates do not
-- wake the agent for a turn the limit would fail.
ALTER TABLE space_turn_retries ADD COLUMN resets_at INTEGER;
UPDATE space_turn_retries SET resets_at=(SELECT u.resets_at FROM execution_upstream_resets u
    WHERE u.execution_id=space_turn_retries.execution_id)
    WHERE delivered=0;
