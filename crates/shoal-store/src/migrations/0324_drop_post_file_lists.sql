-- `thread_post_files` lists each post's files, which the migration's
-- preparation filled from these lists.
ALTER TABLE thread_posts DROP COLUMN files;
