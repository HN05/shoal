-- The gate's sessions, one per space and subject: the space's change, its
-- issue proposals for one repository, or its pushes to one other ref. Each
-- keeps the agent and the rules it opened with, which a later request must
-- share to resume it, and the commit its last verdict saw. They go with the
-- space.
CREATE TABLE gate_sessions (
    space_id TEXT NOT NULL,
    subject TEXT NOT NULL,
    agent TEXT NOT NULL,
    -- The JSON rules the session's opening gave it.
    rules TEXT NOT NULL,
    session TEXT NOT NULL,
    seen TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(space_id, subject)
);

-- Each verdict the gate gave an action of a space, which counts the denials
-- that stop asking; they go with the space.
CREATE TABLE gate_verdicts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    space_id TEXT NOT NULL,
    action TEXT NOT NULL,
    verdict TEXT NOT NULL CHECK(verdict IN ('allow','deny','escalate','failed')),
    reason TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX gate_verdicts_action ON gate_verdicts(space_id, action);

CREATE TRIGGER workspace_gate_removed AFTER DELETE ON workspaces BEGIN
    DELETE FROM gate_sessions WHERE space_id=OLD.id;
    DELETE FROM gate_verdicts WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_gate_removed AFTER DELETE ON chats BEGIN
    DELETE FROM gate_sessions WHERE space_id=OLD.id;
    DELETE FROM gate_verdicts WHERE space_id=OLD.id;
END;
