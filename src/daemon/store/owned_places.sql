-- Places Shoal created for a workspace, such as Rex blocks, which it closes
-- once the workspace is removed. A record outlives its workspace until the
-- place is closed: for a host without ownership labels it is the only proof
-- that a place is Shoal's to close. A place a chat took over belongs to no
-- workspace, so nothing closes it.
CREATE TABLE owned_places (
    host TEXT NOT NULL,
    place TEXT NOT NULL,
    workspace_id TEXT,
    origin TEXT NOT NULL,
    PRIMARY KEY (host, place)
);
-- A pending close keeps how to reach its place's host.
ALTER TABLE session_cleanup ADD COLUMN origin TEXT;
