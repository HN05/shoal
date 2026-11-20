-- A move of the space `space_id` and the spaces it carries to
-- `destination`, recorded before any agent stops, so a daemon restarted
-- during it finishes it from `phase`. Each JSON column holds what the
-- protocol's `MoveRecord` names it.
CREATE TABLE space_moves (
    space_id TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    destination TEXT NOT NULL,
    carrying TEXT NOT NULL,
    unsaved TEXT NOT NULL,
    phase TEXT NOT NULL CHECK(phase IN ('packing','delivering','delivered')),
    checkpoints TEXT NOT NULL DEFAULT '{}',
    saved_before TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL
);
