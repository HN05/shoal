-- Workspaces that attempt the same task share its name in `swarm` within one
-- repository. A swarm needs two workspaces, so removing the second-to-last
-- one clears the name from the workspace that remains.
ALTER TABLE workspaces ADD COLUMN swarm TEXT;
CREATE INDEX IF NOT EXISTS workspaces_swarm ON workspaces(repository_id, swarm);

CREATE TRIGGER IF NOT EXISTS workspace_swarm_removed AFTER DELETE ON workspaces
WHEN OLD.swarm IS NOT NULL BEGIN
    UPDATE workspaces SET swarm=NULL
    WHERE repository_id=OLD.repository_id AND swarm=OLD.swarm
        AND (SELECT COUNT(*) FROM workspaces
             WHERE repository_id=OLD.repository_id AND swarm=OLD.swarm)=1;
END;
