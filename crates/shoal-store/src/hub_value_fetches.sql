-- The settings whose value each machine fetched from another through the
-- hub, never the value: when it first and last did. Revoking the machine
-- names them, since what it received cannot be taken back.
CREATE TABLE hub_value_fetches (
    requester TEXT NOT NULL REFERENCES hub_members(id),
    machine TEXT NOT NULL REFERENCES hub_members(id),
    setting TEXT NOT NULL,
    first_at INTEGER NOT NULL,
    last_at INTEGER NOT NULL,
    PRIMARY KEY(requester, machine, setting)
);
