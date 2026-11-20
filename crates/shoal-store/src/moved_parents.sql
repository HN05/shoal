-- The spaces a move to a joined machine leaves here, each with the space it
-- is below, detached while that space's removal here runs and linked to it
-- again once the removal ended, also after a restart during it.
CREATE TABLE moved_parents (
    space_id TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    parent_id TEXT NOT NULL
);
