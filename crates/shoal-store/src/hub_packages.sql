-- The latest package of each space of an intermittent machine that the hub
-- keeps, whose bytes lie in its state directory under the space's ID: the
-- machine that sent it, and when that machine built it, so what changed
-- since is not in it. It goes once the space ends on that machine.
CREATE TABLE hub_packages (
    space_id TEXT PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    built_at INTEGER NOT NULL,
    size INTEGER NOT NULL,
    kept_at INTEGER NOT NULL
);
CREATE INDEX hub_packages_member ON hub_packages(member_id);

-- A space ends on its machine when the directory drops it there, as once
-- that machine reports it no longer has it or the hub revokes the machine.
CREATE TRIGGER hub_packages_ended AFTER DELETE ON hub_spaces BEGIN
    DELETE FROM hub_packages
        WHERE space_id=OLD.space_id AND member_id=OLD.member_id;
END;

-- Nor does a machine keep a package of a space whose ID another machine
-- took over, as by a move or a takeover.
CREATE TRIGGER hub_packages_handed_over AFTER UPDATE OF member_id ON hub_spaces
    WHEN OLD.member_id <> NEW.member_id BEGIN
    DELETE FROM hub_packages
        WHERE space_id=OLD.space_id AND member_id=OLD.member_id;
END;

-- What a machine sent the hub of each of its spaces: the state the last
-- package that arrived holds, none before one did, when it last tried to
-- send one, and since when the space has changes the hub's package lacks,
-- none while it is current. It goes with the space.
CREATE TABLE hub_package_states (
    space_id TEXT PRIMARY KEY,
    sent TEXT,
    tried_at INTEGER,
    unsent_since INTEGER
);

CREATE TRIGGER hub_package_states_removed AFTER DELETE ON spaces BEGIN
    DELETE FROM hub_package_states WHERE space_id=OLD.id;
END;
