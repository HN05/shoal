-- When each machine announced its sleep, or ran out of battery as its last
-- report said it would, until its link opens again: the hub takes its
-- spaces over once it has stayed away long enough.
ALTER TABLE hub_members ADD COLUMN asleep_at INTEGER;

-- The spaces of a sleeping machine whose packages lacked changes as it
-- went to sleep, with since when, none where that is unknown, until a
-- package its machine built after arrives.
CREATE TABLE hub_sleep_lags (
    space_id TEXT PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    since INTEGER
);
CREATE INDEX hub_sleep_lags_member ON hub_sleep_lags(member_id);
