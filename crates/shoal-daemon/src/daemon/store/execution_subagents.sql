-- The subagents an execution's agent started, by the ID its AI tool names
-- each by, with how often its start and its stop were reported and when it
-- last started, in Unix seconds. A subagent resumed under its ID starts
-- again; one runs while it started more often than it stopped, so a stop
-- that arrives before its start leaves it stopped. They go with the
-- execution.
CREATE TABLE execution_subagents (
    execution_id TEXT NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    starts INTEGER NOT NULL DEFAULT 0,
    stops INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (execution_id, agent_id)
);

-- When a request of the subagents of an execution's agent last passed its
-- proxy, in Unix seconds.
CREATE TABLE execution_subagent_requests (
    execution_id TEXT PRIMARY KEY REFERENCES executions(id) ON DELETE CASCADE,
    at INTEGER NOT NULL
);
