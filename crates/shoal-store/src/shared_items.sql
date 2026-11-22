-- What a hub's machines share, of every kind, each item under its kind's
-- key and with its value in JSON. On the hub, each item's copy: the last
-- edit of it that reached the hub, at a revision later edits of any item
-- exceed; a removed item keeps its row without a value, so an older copy
-- never brings it back.
CREATE TABLE hub_shared_items (
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT,
    revision INTEGER NOT NULL,
    PRIMARY KEY(kind, key)
);
INSERT INTO hub_shared_items(kind, key, value, revision)
    SELECT 'file', file, CASE WHEN text IS NULL THEN NULL ELSE json_quote(text) END, revision
    FROM hub_shared_files;
DROP TABLE hub_shared_files;
-- On every machine of a hub, the hub's own included, the copy of each item
-- it last agreed on with the hub whose key `hub_key` is, sent or received:
-- an item that differs from it holds a local edit.
CREATE TABLE shared_items (
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    hub_key TEXT NOT NULL,
    value TEXT,
    revision INTEGER NOT NULL,
    PRIMARY KEY(kind, key)
);
INSERT INTO shared_items(kind, key, hub_key, value, revision)
    SELECT 'file', file, hub_key, CASE WHEN text IS NULL THEN NULL ELSE json_quote(text) END, revision
    FROM shared_files;
DROP TABLE shared_files;
