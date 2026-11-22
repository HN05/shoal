-- On the hub, the repositories each machine reported it registers, as
-- its last report had them, with the ID that machine gives each.
CREATE TABLE hub_repositories (
    member_id TEXT NOT NULL REFERENCES hub_members(id),
    repository_id TEXT NOT NULL,
    summary TEXT NOT NULL,
    PRIMARY KEY(member_id, repository_id)
);
