-- The numbers the hub keeps for a joined machine beyond those its listed
-- spaces hold: spares the machine numbers its new spaces from, and the
-- number a space of the machine takes instead of one another machine's
-- space held first, until the machine reports a space holding it.
CREATE TABLE hub_numbers (
    number INTEGER PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    space_id TEXT UNIQUE
);
CREATE INDEX hub_numbers_member ON hub_numbers(member_id);
