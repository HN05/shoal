-- Marks the token a chat's agent carries, which lasts as long as the chat,
-- apart from the tokens any space issues for other processes, which until
-- now only a workspace issued.
ALTER TABLE space_scopes ADD COLUMN agent INTEGER NOT NULL DEFAULT 0;
UPDATE space_scopes SET agent=1 WHERE space_id IN (SELECT id FROM chats);
