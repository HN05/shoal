-- What each joined machine keeps of removed spaces, as it last reported it:
-- their removal records, which a move of one reads there, and their
-- attached files, which a request for them reaches there.
CREATE TABLE hub_kept_removed (
    space_id TEXT NOT NULL,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    keeps TEXT NOT NULL CHECK(keeps IN ('record','files')),
    PRIMARY KEY(space_id, member_id, keeps)
);

INSERT INTO hub_kept_removed(space_id, member_id, keeps)
    SELECT space_id, member_id, 'files' FROM hub_kept_files;

DROP TABLE hub_kept_files;
