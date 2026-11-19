-- Every space, a workspace or a chat, which has no branch and may have no
-- repository. Queries that need every space read it here.
CREATE VIEW spaces AS
    SELECT id, repository_id, name, 'workspace' AS kind, number, branch FROM workspaces
    UNION ALL SELECT id, repository_id, name, 'chat', number, NULL FROM chats;

-- The views that list spaces read them from it.
DROP VIEW space_numbers;
CREATE VIEW space_numbers AS
    SELECT number FROM spaces WHERE number IS NOT NULL
    UNION ALL SELECT space_number FROM access_requests WHERE workspace_id IS NULL;
DROP VIEW journal_spaces;
CREATE VIEW journal_spaces AS
    SELECT spaces.id,
        CASE kind WHEN 'workspace' THEN spaces.id END AS workspace_id,
        CASE kind WHEN 'chat' THEN spaces.id END AS chat_id,
        spaces.repository_id, spaces.name, workspaces.path, spaces.branch
    FROM spaces LEFT JOIN workspaces USING(id);
