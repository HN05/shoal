-- The runner each repository registered on its forge to run its CI jobs on
-- this machine: the forge repository it serves, the ID that deletes it, and
-- what it connects as.
CREATE TABLE IF NOT EXISTS runners (
    repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
    forge_repository TEXT NOT NULL,
    forge_id INTEGER NOT NULL,
    uuid TEXT NOT NULL,
    token TEXT NOT NULL
);
