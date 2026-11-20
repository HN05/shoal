-- The moves to a joined machine that would stop the agent of `space_id`,
-- which asked for them, recorded until its turn ends, when Shoal starts
-- them and keeps the operation that runs them here until it ends, so it
-- tells the agent how a move that left its space here went.
CREATE TABLE pending_moves (
    space_id TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    move_order TEXT NOT NULL,
    operation_id TEXT,
    created_at INTEGER NOT NULL
);
