-- The number the hub keeps for a space this machine imports as another
-- machine of the hub moves it here, which the space takes when it is
-- recorded here while no space here holds it.
CREATE TABLE hub_kept_numbers (
    space_id TEXT PRIMARY KEY,
    number INTEGER NOT NULL
);
