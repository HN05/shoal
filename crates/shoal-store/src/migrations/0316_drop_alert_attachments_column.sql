-- The migration's preparation moved each alert's attached paths into
-- alert_attachments.
ALTER TABLE alerts DROP COLUMN attachments;
