-- Every space's holds, a chat's as a workspace's, which keep it from every
-- unattended removal until released; they go with the space and stay
-- through its conversion to the other kind.
CREATE TABLE space_holds (
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    reason TEXT,
    created_at INTEGER NOT NULL,
    placed_by TEXT NOT NULL CHECK(placed_by IN ('user','agent')),
    PRIMARY KEY(space_id,name)
);
INSERT INTO space_holds(space_id,name,reason,created_at,placed_by)
    SELECT workspace_id,name,reason,created_at,placed_by FROM workspace_holds;
DROP TABLE workspace_holds;
