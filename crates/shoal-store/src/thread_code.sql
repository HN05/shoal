-- The places in code a thread's post points at. `space_id` names the space
-- whose worktree it is in, and `space_name` keeps its name for when the
-- space is gone; the repository keeps its file versions under that space's
-- ref. `path`, `start`, `end`, `blob` and `snippet` are where it was made
-- and never change; the `latest_` columns and `state` are where Shoal last
-- followed it to. Lines count from 0, `end` excluded; a reference without
-- them points at a whole file or directory, and one without a blob at a
-- directory, a binary file or a large one.
CREATE TABLE thread_code (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    post_id INTEGER NOT NULL REFERENCES thread_posts(id) ON DELETE CASCADE,
    space_id TEXT NOT NULL,
    space_name TEXT NOT NULL,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    commit_id TEXT,
    path TEXT NOT NULL,
    start INTEGER,
    end INTEGER,
    blob TEXT,
    snippet TEXT,
    latest_path TEXT NOT NULL,
    latest_start INTEGER,
    latest_end INTEGER,
    latest_blob TEXT,
    state TEXT NOT NULL CHECK(state IN ('unchanged','changed','outdated')),
    CHECK((start IS NULL) = (end IS NULL)
        AND (latest_start IS NULL) = (latest_end IS NULL)
        AND (start IS NULL) = (latest_start IS NULL))
);
CREATE INDEX thread_code_post ON thread_code(post_id);
CREATE INDEX thread_code_space ON thread_code(space_id);
CREATE INDEX thread_code_repository ON thread_code(repository_id);
