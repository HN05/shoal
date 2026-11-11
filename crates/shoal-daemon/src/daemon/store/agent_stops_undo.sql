-- What tests that emulate an earlier database run first to undo
-- agent_stops.sql: agents' stops live in memory again.
DROP TABLE agent_stops;
