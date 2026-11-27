-- A conversion keeps the kind of space it makes and the preparation its
-- agent waits for in columns, which migration 314's preparation fills from
-- the records.
CREATE TABLE space_conversions_columns (
    space_id TEXT PRIMARY KEY,
    target TEXT NOT NULL,
    preparation TEXT,
    record TEXT NOT NULL
);
