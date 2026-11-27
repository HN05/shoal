-- A pending worktree's plan keeps its source's kind and the checkpoint the
-- source names in columns, which queries read, rather than in its JSON. The
-- migration fills them from the plans stored before.
ALTER TABLE workspaces ADD COLUMN pending_source TEXT;
ALTER TABLE workspaces ADD COLUMN pending_checkpoint INTEGER;
