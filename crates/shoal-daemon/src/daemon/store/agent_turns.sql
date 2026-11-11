-- A space's agent state counts the turns its reports started, and the
-- reports that came from another execution, so a decision made while the
-- agent was idle can tell whether a turn started since in the transaction
-- that acts on it. It also keeps whether an interrupt ended the last turn,
-- until a turn starts or ends again.
ALTER TABLE space_agent_state ADD COLUMN turn INTEGER NOT NULL DEFAULT 0;
ALTER TABLE space_agent_state ADD COLUMN interrupted INTEGER NOT NULL DEFAULT 0;

-- The count a space's state reached when it went, as with its execution or
-- a chat's stop, which its next state continues, so a count is never reused
-- while the space lasts, through its conversion to the other kind.
CREATE TABLE space_turns (
    space_id TEXT PRIMARY KEY,
    turn INTEGER NOT NULL
);

CREATE TRIGGER space_agent_state_turn_kept AFTER DELETE ON space_agent_state
WHEN OLD.space_id IN (SELECT id FROM workspaces UNION ALL SELECT id FROM chats) BEGIN
    INSERT INTO space_turns(space_id,turn) VALUES (OLD.space_id,OLD.turn)
        ON CONFLICT(space_id) DO UPDATE SET turn=max(turn,excluded.turn);
END;

CREATE TRIGGER workspace_turns_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_turns WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_turns_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_turns WHERE space_id=OLD.id;
END;
