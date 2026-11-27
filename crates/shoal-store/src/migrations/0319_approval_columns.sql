-- An approval request keeps what queries read in columns: its status,
-- the space that decides it, the gate's step, the operation pushing what
-- was approved, and the space that proposed an issue its repository owns.
-- Migration 320 fills them from the records and drops those fields there.
CREATE TABLE access_request_columns (
    id TEXT PRIMARY KEY,
    space_id TEXT REFERENCES spaces(id) ON DELETE CASCADE,
    repository_id TEXT REFERENCES repositories(id) ON DELETE CASCADE,
    proposer_id TEXT,
    target_key TEXT NOT NULL,
    name TEXT NOT NULL,
    status TEXT NOT NULL,
    decider TEXT,
    gate TEXT,
    operation TEXT,
    record TEXT NOT NULL,
    active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
    number INTEGER NOT NULL DEFAULT 0,
    space_number INTEGER NOT NULL DEFAULT 0,
    CHECK((space_id IS NULL) <> (repository_id IS NULL)),
    CHECK((repository_id IS NULL) = (proposer_id IS NULL))
);
