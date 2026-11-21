-- The shared config's files across a hub's machines. On the hub, each
-- file's copy: the last edit of it that reached the hub, at a revision
-- later edits of any file exceed; a removed file keeps its row without
-- text, so an older copy never brings it back.
CREATE TABLE hub_shared_files (
    file TEXT PRIMARY KEY,
    text TEXT,
    revision INTEGER NOT NULL
);
-- On every machine of a hub, the hub's own included, the copy of each file
-- it last agreed on with the hub whose key `hub_key` is, sent or received:
-- a file on disk that differs from it holds a local edit.
CREATE TABLE shared_files (
    file TEXT PRIMARY KEY,
    hub_key TEXT NOT NULL,
    text TEXT,
    revision INTEGER NOT NULL
);
