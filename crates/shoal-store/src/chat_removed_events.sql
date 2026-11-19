-- A chat's removal is journaled as `removed`, with its cause, by the
-- removal that deletes its record, as a workspace's is.
DROP TRIGGER chat_ended;
