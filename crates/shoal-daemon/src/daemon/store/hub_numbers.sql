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

-- The numbers a joined machine holds without a space, as for a removed
-- space's issue proposals, which it reports with its spaces and the hub
-- gives no other space.
CREATE TABLE hub_reserved_numbers (
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    number INTEGER NOT NULL,
    PRIMARY KEY(member_id, number)
);

-- The numbers each machine holds without a listed space of its own: those
-- it reports holding without a space, and those of its copies of spaces
-- another machine owns, each copy with its space's ID.
CREATE VIEW hub_held_numbers AS
    SELECT member_id, number, NULL AS space_id FROM hub_reserved_numbers
    UNION ALL SELECT member_id, json_extract(summary,'$.number'), space_id
        FROM hub_space_copies;
