-- Where an execution or a chat runs in a terminal host, so attach and
-- status can ask that host whether the place is still open.
ALTER TABLE executions ADD COLUMN origin TEXT;
ALTER TABLE chats ADD COLUMN origin TEXT;
