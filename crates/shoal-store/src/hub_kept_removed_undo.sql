-- What tests that emulate an earlier database run first to undo
-- hub_kept_removed.sql.
CREATE TABLE hub_kept_files (
    space_id TEXT NOT NULL,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    PRIMARY KEY(space_id, member_id)
);

INSERT INTO hub_kept_files(space_id, member_id)
    SELECT space_id, member_id FROM hub_kept_removed WHERE keeps='files';

DROP TABLE hub_kept_removed;
