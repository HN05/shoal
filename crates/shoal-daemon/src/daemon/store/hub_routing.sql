-- What the hub holds for a joined machine while it is offline: requests
-- and events for its spaces, in the order they came, each with the
-- machine and space that sent it, which hear of it once it expires, and
-- whether the hub has sent it without hearing back, which it then never
-- sends again.
CREATE TABLE hub_queue (
    seq INTEGER PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    sender_id TEXT NOT NULL,
    sender_space TEXT,
    call TEXT NOT NULL,
    summary TEXT NOT NULL,
    queued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    delivering INTEGER NOT NULL DEFAULT 0 CHECK(delivering IN (0, 1))
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

-- The questions whose coordinator's machine stayed offline too long, which
-- went on to the next space above that is online, or to the user, once.
CREATE TABLE hub_escalated_questions (
    space_id TEXT NOT NULL,
    number INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(space_id, number, created_at)
);

-- What a joined machine last learned of the spaces of other machines and
-- how they hang together, which a removal goes by while the hub cannot be
-- asked, across restarts too.
CREATE TABLE hub_known_tree (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    tree TEXT NOT NULL
);
