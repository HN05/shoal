-- The viewer the daemon started for a simulator demo: the port it serves on
-- and its process, by whose identity the daemon knows it after a restart.
CREATE TABLE demo_viewers (
    execution_id TEXT PRIMARY KEY REFERENCES demos(execution_id) ON DELETE CASCADE,
    port INTEGER NOT NULL,
    process TEXT NOT NULL
);
