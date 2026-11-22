-- What tests that emulate an earlier database run first to undo
-- shared_items.sql.
DROP TABLE shared_items;
DROP TABLE hub_shared_items;
CREATE TABLE hub_shared_files (
    file TEXT PRIMARY KEY,
    text TEXT,
    revision INTEGER NOT NULL
);
CREATE TABLE shared_files (
    file TEXT PRIMARY KEY,
    hub_key TEXT NOT NULL,
    text TEXT,
    revision INTEGER NOT NULL
);
