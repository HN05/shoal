-- What a kept package continues its space with: the start its agent would
-- make, which the hub places a takeover of the space by, and for a chat in
-- a workspace's worktree, that workspace, which it follows. A package kept
-- before has neither, so its machine sends each space again.
ALTER TABLE hub_packages ADD COLUMN start TEXT;
ALTER TABLE hub_packages ADD COLUMN within TEXT;
UPDATE hub_package_states SET sent=NULL;
