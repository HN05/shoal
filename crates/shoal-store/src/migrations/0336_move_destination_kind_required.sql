-- Every move's destination has a kind.
CREATE TABLE space_moves_kinded (
    space_id TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    destination TEXT NOT NULL,
    destination_kind TEXT NOT NULL,
    ticket TEXT NOT NULL,
    carrying TEXT NOT NULL,
    unsaved TEXT NOT NULL,
    phase TEXT NOT NULL CHECK(phase IN ('packing','delivering','delivered')),
    checkpoints TEXT NOT NULL DEFAULT '{}',
    saved_before TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL,
    held_until INTEGER
);

INSERT INTO space_moves_kinded(space_id,destination,destination_kind,ticket,carrying,unsaved,
        phase,checkpoints,saved_before,created_at,held_until)
    SELECT space_id,destination,destination_kind,ticket,carrying,unsaved,phase,checkpoints,
        saved_before,created_at,held_until
    FROM space_moves;

DROP TABLE space_moves;
ALTER TABLE space_moves_kinded RENAME TO space_moves;
