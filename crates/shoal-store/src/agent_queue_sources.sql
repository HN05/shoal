-- What made a launch, as JSON, which its agent's start records; none for
-- a restore, or a launch queued before launches recorded it.
ALTER TABLE agent_queue ADD COLUMN source TEXT;
