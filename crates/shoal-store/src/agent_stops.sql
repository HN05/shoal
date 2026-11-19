-- The protection stop Shoal decided for an agent, by the execution that runs
-- it, until the agent runs again, its restore is cancelled or the execution
-- goes: the reason, what its restore waits for, the turn generation of its
-- space the stop was decided at, and the space whose process asked for it,
-- if one did. One left by an earlier daemon authorizes no signal; a wrapper
-- that reattaches says whether its restore still waits.
CREATE TABLE agent_stops (
    execution_id TEXT PRIMARY KEY REFERENCES executions(id) ON DELETE CASCADE,
    reason TEXT NOT NULL,
    resumes_when TEXT NOT NULL,
    turn INTEGER,
    stopped_by TEXT
);
