-- As the hub: what other machines did to a machine's alert to the user
-- that the machine has not reported doing yet: `read_marked` while its
-- copy is read only there, and `deleted` while it is cleared only there,
-- which leaves its copy out.
ALTER TABLE hub_user_alerts ADD COLUMN read_marked INTEGER NOT NULL DEFAULT 0
    CHECK(read_marked IN (0,1));
ALTER TABLE hub_user_alerts ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0
    CHECK(deleted IN (0,1));
