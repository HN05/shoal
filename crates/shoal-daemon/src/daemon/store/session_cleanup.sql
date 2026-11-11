-- Terminal closes outlive the execution and workspace that requested them.
CREATE TABLE session_cleanup (
    id TEXT PRIMARY KEY,
    host TEXT NOT NULL,
    name TEXT NOT NULL,
    owner TEXT NOT NULL
);
