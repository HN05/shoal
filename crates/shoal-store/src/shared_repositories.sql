-- The URL the other machines of a hub clone a repository from, while it is
-- shared with them.
ALTER TABLE repositories ADD COLUMN shared_url TEXT;
