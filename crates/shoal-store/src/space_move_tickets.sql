-- Each recorded move holds the ticket that a machine's claims of its
-- spaces carry on a move through the hub; a move recorded before gets one
-- of its own.
CREATE TABLE space_moves_ticketed (
    space_id TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    destination TEXT NOT NULL,
    ticket TEXT NOT NULL,
    carrying TEXT NOT NULL,
    unsaved TEXT NOT NULL,
    phase TEXT NOT NULL CHECK(phase IN ('packing','delivering','delivered')),
    checkpoints TEXT NOT NULL DEFAULT '{}',
    saved_before TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL
);
INSERT INTO space_moves_ticketed(space_id,destination,ticket,carrying,unsaved,phase,checkpoints,
        saved_before,created_at)
    SELECT space_id,destination,lower(hex(randomblob(16))),carrying,unsaved,phase,checkpoints,
        saved_before,created_at FROM space_moves;
DROP TABLE space_moves;
ALTER TABLE space_moves_ticketed RENAME TO space_moves;
