-- A space's blockers are rows of their own, in the order the plan keeps
-- them, which queries read, rather than an array in its plan's JSON. A
-- step's blocker names the step by its key, or by its name without one.
-- The migration moves them from the plans stored before.
CREATE TABLE space_blockers (
    space_id TEXT NOT NULL REFERENCES space_progress(space_id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    kind TEXT NOT NULL,
    target TEXT NOT NULL,
    name TEXT NOT NULL,
    reason TEXT NOT NULL,
    lookup_failed TEXT,
    step_key TEXT,
    step_name TEXT CHECK(step_key IS NULL OR step_name IS NULL),
    PRIMARY KEY(space_id, position)
);
