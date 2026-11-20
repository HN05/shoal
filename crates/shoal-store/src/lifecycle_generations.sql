-- How many times each workspace's lifecycle state has changed, which a
-- command's start compares with the count it read first to tell that a
-- lifecycle operation came and went while it registered.
ALTER TABLE workspaces ADD COLUMN lifecycle_generation INTEGER NOT NULL DEFAULT 0;

CREATE TRIGGER workspace_lifecycle_changed AFTER UPDATE OF state ON workspaces
WHEN NEW.state IS NOT OLD.state BEGIN
    UPDATE workspaces SET lifecycle_generation=lifecycle_generation+1 WHERE id=NEW.id;
END;
