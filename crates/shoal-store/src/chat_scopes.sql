-- Every chat's agent carries a scope token, as a workspace's tracked
-- execution does, which confines it to the chat and the spaces below it. It
-- lasts as long as the chat, across stops and restores. A chat already
-- recorded gets one now, which its agent carries from its next restore.
ALTER TABLE chats ADD COLUMN scope_token TEXT;
UPDATE chats SET scope_token=lower(hex(randomblob(16)));
CREATE UNIQUE INDEX chats_scope_token ON chats(scope_token);
