-- The hub's directory keeps a space's ID and number, which its queries
-- match, in columns, and each numbering keeps its spares in a table. The
-- next migration's preparation fills them from the JSON that held them.
CREATE TABLE hub_spaces_numbered (
    space_id TEXT PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    number INTEGER NOT NULL,
    summary TEXT NOT NULL,
    reported_at INTEGER NOT NULL
);

CREATE TABLE hub_space_copies_numbered (
    space_id TEXT NOT NULL,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    number INTEGER NOT NULL,
    summary TEXT NOT NULL,
    since INTEGER NOT NULL,
    handed_over INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(space_id, member_id)
);

CREATE TABLE hub_numbering_spares (number INTEGER PRIMARY KEY);

CREATE TABLE hub_alert_numbering_spares (number INTEGER PRIMARY KEY);
