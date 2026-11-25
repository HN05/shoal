-- Surfaces and answers replace choices and picks, which the migration's
-- preparation rewrote into them.
ALTER TABLE thread_posts DROP COLUMN choices;
ALTER TABLE thread_posts DROP COLUMN recommendation;
ALTER TABLE thread_posts DROP COLUMN reason;
ALTER TABLE thread_posts DROP COLUMN choice;
