-- The removed spaces whose attached files each joined machine keeps, as it
-- last reported them, so that a request for those files reaches it.
CREATE TABLE hub_kept_files (
    space_id TEXT NOT NULL,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    PRIMARY KEY(space_id, member_id)
);
