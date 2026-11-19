-- The entries apps keep in Shoal, each app's under keys of its own: shared
-- across the user's machines and devices, with an empty device, or one
-- device's, this machine's own with an empty device and, as the hub, a
-- paired device's by its member ID. Shoal never reads the bytes.
CREATE TABLE app_entries (
    app TEXT NOT NULL,
    scope TEXT NOT NULL CHECK(scope IN ('shared', 'device')),
    device TEXT NOT NULL DEFAULT '' CHECK(scope = 'device' OR device = ''),
    key TEXT NOT NULL,
    content_type TEXT NOT NULL,
    bytes BLOB NOT NULL,
    revision INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(app, scope, device, key)
);

-- The revision the latest change of an entry took; the next takes the one
-- after it, so revisions never repeat, even for a key deleted and written
-- again.
CREATE TABLE app_revisions (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    revision INTEGER NOT NULL
);
INSERT INTO app_revisions(id, revision) VALUES (1, 0);
