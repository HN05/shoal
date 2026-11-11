-- What tests that emulate an earlier database run first to undo
-- agent_turns.sql: agent states count no turns, and an agent's interrupt
-- lives in memory again.
DROP TRIGGER space_agent_state_turn_kept;
DROP TRIGGER workspace_turns_removed;
DROP TRIGGER chat_turns_removed;
DROP TABLE space_turns;
ALTER TABLE space_agent_state DROP COLUMN turn;
ALTER TABLE space_agent_state DROP COLUMN interrupted;
