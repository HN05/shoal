-- How this machine numbers its new spaces while it takes part in a hub's
-- sequence: from `spares`, the numbers the hub keeps for it, as a JSON
-- array, the smallest no space here holds first, and once none is left,
-- above its own highest number. Without it, a new space takes the
-- smallest number free here.
CREATE TABLE hub_numbering (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    spares TEXT NOT NULL
);
