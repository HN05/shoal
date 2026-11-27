-- The files a thread's post sent, by their records, in the order it sent
-- them. A row outlives its file's record, which a read then leaves out.
-- Migration 324 fills it from each post's list and drops the list.
CREATE TABLE thread_post_files (
    post_id INTEGER NOT NULL REFERENCES thread_posts(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    file_id INTEGER NOT NULL,
    PRIMARY KEY(post_id, position)
);
CREATE INDEX thread_post_files_file ON thread_post_files(file_id);
