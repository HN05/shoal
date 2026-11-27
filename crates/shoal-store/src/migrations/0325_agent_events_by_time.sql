-- Placement reads how long each repository's agents ran over the last
-- week, by the starts and ends of agents in that time.
CREATE INDEX space_events_agents ON space_events(created_at)
    WHERE kind IN ('agent_started','agent_stopped','agent_exited');
