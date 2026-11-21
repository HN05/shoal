-- The spaces another machine took over from the hub's package of them
-- while their own machine was offline: that machine, which retires its
-- copy once its link opens, when the package the takeover went by was
-- built, so that machine tells whether its copy changed after, and when
-- the takeover's claim came.
CREATE TABLE hub_takeovers (
    space_id TEXT PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    built_at INTEGER NOT NULL,
    taken_at INTEGER NOT NULL
);
CREATE INDEX hub_takeovers_member ON hub_takeovers(member_id);
