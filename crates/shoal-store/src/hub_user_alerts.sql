-- As the hub: a copy of every machine's alerts to the user, its own
-- machine's included, as each machine reports them, under the machine that
-- keeps it; `alert` is the alert's JSON, naming that machine.
CREATE TABLE hub_user_alerts (
    member_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    alert TEXT NOT NULL,
    PRIMARY KEY(member_id, id)
);
