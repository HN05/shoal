-- A post's surface, an A2UI createSurface message, and its answer to the
-- other side's surface, as `{"post": ..., "action": ..., "values": {...}}`,
-- both JSON.
ALTER TABLE thread_posts ADD COLUMN ui TEXT;
ALTER TABLE thread_posts ADD COLUMN answer TEXT;
