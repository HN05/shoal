-- What the hub holds for a joined machine while it is offline: requests
-- and events for its spaces, in the order they came, each with the
-- machine and space that sent it, which hear of it once it expires.
CREATE TABLE hub_queue (
    seq INTEGER PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    sender_id TEXT NOT NULL,
    sender_space TEXT,
    call TEXT NOT NULL,
    summary TEXT NOT NULL,
    queued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX hub_queue_member ON hub_queue(member_id, seq);

-- What a joined machine sends the hub once its link opens again: the
-- events its spaces passed up to spaces on other machines meanwhile, in
-- the order they came.
CREATE TABLE hub_outbox (
    seq INTEGER PRIMARY KEY,
    call TEXT NOT NULL,
    queued_at INTEGER NOT NULL
);
