-- A removal record keeps the workspace a chat ran in and the expiry of a
-- workspace's checkpoint in columns, which queries read and restores
-- change, and leaves them and the fields its other columns keep out of its
-- JSON. The migration moves them from the records stored before.
ALTER TABLE removed_spaces ADD COLUMN chat_workspace_id TEXT;
ALTER TABLE removed_spaces ADD COLUMN checkpoint_expires_at INTEGER;
