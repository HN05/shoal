CREATE TABLE failed_launches (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL UNIQUE REFERENCES spaces(id) ON DELETE CASCADE,
    repository_id TEXT REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    agent TEXT NOT NULL,
    issue_title TEXT,
    issue_labels TEXT,
    plan TEXT NOT NULL,
    env TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    mode TEXT,
    narrowing TEXT,
    scope_space_id TEXT,
    priority INTEGER CHECK(priority BETWEEN 0 AND 100),
    source TEXT,
    error TEXT NOT NULL,
    failed_at INTEGER NOT NULL
);
