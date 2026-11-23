-- How this machine numbers its new alerts while it takes part in a hub's
-- sequence: from `spares`, the numbers the hub keeps for it, as a JSON
-- array, each above `taken`, the highest it took, and once none is left,
-- above the highest it took, holds or saw another machine's alert take,
-- which waits in `hub_unconfirmed_alerts` until the hub confirms it or
-- gives the alert another. Without it, the store numbers a new alert.
CREATE TABLE hub_alert_numbering (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    spares TEXT NOT NULL,
    taken INTEGER NOT NULL,
    seen INTEGER NOT NULL
);
CREATE TABLE hub_unconfirmed_alerts (number INTEGER PRIMARY KEY);

-- As the hub: the next number of its one sequence of alert numbers, which
-- it never gives twice, and the numbers it keeps for each machine: its
-- spares, and those its alerts took that the hub took for them, until the
-- machine reports that it heard.
CREATE TABLE hub_alert_sequence (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    next INTEGER NOT NULL
);
CREATE TABLE hub_alert_numbers (
    number INTEGER PRIMARY KEY,
    member_id TEXT NOT NULL,
    spare INTEGER NOT NULL CHECK(spare IN (0,1))
);
CREATE INDEX hub_alert_numbers_member ON hub_alert_numbers(member_id, spare);
