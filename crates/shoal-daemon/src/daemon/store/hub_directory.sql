-- The hub's directory: the summary each joined machine last reported of
-- each of its spaces, as JSON, under the machine that owns the space's ID,
-- the first that reported it.
CREATE TABLE hub_spaces (
    space_id TEXT PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    summary TEXT NOT NULL,
    reported_at INTEGER NOT NULL
);
CREATE INDEX hub_spaces_member ON hub_spaces(member_id);

-- The spaces machines report under an ID another owns, which the hub
-- leaves out of its directory until the owner's space ends, when the
-- earliest copy takes its place.
CREATE TABLE hub_space_copies (
    space_id TEXT NOT NULL,
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    summary TEXT NOT NULL,
    since INTEGER NOT NULL,
    PRIMARY KEY(space_id, member_id)
);
