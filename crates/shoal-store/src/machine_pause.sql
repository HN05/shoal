-- When the user paused this machine, which admits no new work and hands
-- what it has over until `shoal unpause`; no row while it is not paused.
CREATE TABLE machine_pause (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    paused_at INTEGER NOT NULL
);
