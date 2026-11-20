-- The moves through the hub that their source settled without the
-- destination's claim, by ticket, so a claim that carries the ticket and
-- comes later is refused and the space never runs on both machines.
CREATE TABLE hub_move_fences (
    ticket TEXT PRIMARY KEY,
    settled_at INTEGER NOT NULL
);
