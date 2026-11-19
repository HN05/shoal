-- When each machine of the hub was there over the last week: the spans its
-- link stayed open, or for the hub's own machine its daemon ran with the
-- machine awake, each extended while it lasts.
CREATE TABLE hub_presence (
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    started_at INTEGER NOT NULL,
    ended_at INTEGER NOT NULL,
    PRIMARY KEY(member_id, started_at)
);

-- What each machine last said of itself: whether it has a battery, none
-- before it said, and its `[machine] availability`; and when the hub first
-- saw it there, which tells a new machine.
ALTER TABLE hub_members ADD COLUMN battery INTEGER CHECK(battery IN (0,1));
ALTER TABLE hub_members ADD COLUMN availability TEXT NOT NULL DEFAULT 'auto'
    CHECK(availability IN ('auto','always_on','intermittent'));
ALTER TABLE hub_members ADD COLUMN present_since INTEGER;
