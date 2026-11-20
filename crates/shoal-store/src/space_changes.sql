-- The changes Shoal records as the agents it runs change their worktrees'
-- files: each a hunk of one file, or a generated, binary or large file
-- without hunks, numbered in its space's own sequence, which
-- `space_change_numbers` counts so no number comes back. `author` says who
-- made it, `turn` and `execution_id` the agent's turn, and `thread_id` the
-- thread it is for. `path`, `start`, `end` and `blob` are where its lines
-- were in the version it produced, which the repository keeps under the
-- space's ref, and the `latest_` columns and `state` where Shoal last
-- followed them to; `before_start`, `removed` and `added` hold its diff,
-- the lines as JSON arrays, whose lengths the `_count` columns keep for
-- readings without them. A change outlives its space: one in a thread
-- goes with the thread, one a post points at with the post, and another
-- expires as checkpoints do.
CREATE TABLE space_changes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    space_id TEXT NOT NULL,
    space_name TEXT NOT NULL,
    number INTEGER NOT NULL,
    repository_id TEXT NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    author TEXT NOT NULL CHECK(author IN ('agent','mixed','others')),
    kind TEXT NOT NULL CHECK(kind IN ('text','generated','binary','large')),
    made_at INTEGER NOT NULL,
    turn INTEGER,
    execution_id TEXT,
    thread_id INTEGER REFERENCES threads(id) ON DELETE CASCADE,
    commit_id TEXT,
    path TEXT NOT NULL,
    start INTEGER,
    end INTEGER,
    blob TEXT,
    size INTEGER,
    before_start INTEGER NOT NULL DEFAULT 0,
    removed TEXT NOT NULL DEFAULT '[]',
    added TEXT NOT NULL DEFAULT '[]',
    removed_count INTEGER NOT NULL DEFAULT 0,
    added_count INTEGER NOT NULL DEFAULT 0,
    latest_path TEXT NOT NULL,
    latest_start INTEGER,
    latest_end INTEGER,
    latest_blob TEXT,
    state TEXT NOT NULL CHECK(state IN ('unchanged','changed','outdated')),
    UNIQUE(space_id, number),
    CHECK((start IS NULL) = (end IS NULL)
        AND (latest_start IS NULL) = (latest_end IS NULL)
        AND (start IS NULL) = (latest_start IS NULL))
);
CREATE INDEX space_changes_thread ON space_changes(thread_id);
CREATE INDEX space_changes_repository ON space_changes(repository_id);

CREATE TABLE space_change_numbers (
    space_id TEXT PRIMARY KEY,
    last INTEGER NOT NULL
);

-- The snapshot of a space's worktree that the next change of its agent's
-- commands is diffed against: what the worktree held after the last tool
-- call Shoal followed, with the versions its edit tools wrote since.
CREATE TABLE space_change_bases (
    space_id TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    tree TEXT NOT NULL
);

-- The commands of a space's agent that run now, by the agent's ID for the
-- call, and when each started.
CREATE TABLE space_change_commands (
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    call TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    PRIMARY KEY(space_id, call)
);

-- The files of a space's worktree an editor last reported the user edited
-- or saved, and when, which make a command's change of the same file mixed.
CREATE TABLE space_edit_reports (
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY(space_id, path)
);

-- The thread a space's agent works for now, until it switches or the
-- thread is resolved.
CREATE TABLE space_current_thread (
    space_id TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE
);
CREATE INDEX space_current_thread_thread ON space_current_thread(thread_id);

-- The changes a thread's post points at.
CREATE TABLE thread_post_changes (
    post_id INTEGER NOT NULL REFERENCES thread_posts(id) ON DELETE CASCADE,
    change_id INTEGER NOT NULL REFERENCES space_changes(id) ON DELETE CASCADE,
    PRIMARY KEY(post_id, change_id)
);
CREATE INDEX thread_post_changes_change ON thread_post_changes(change_id);

CREATE TRIGGER space_changes_resolved AFTER UPDATE OF outcome ON threads
WHEN NEW.outcome IS NOT NULL BEGIN
    DELETE FROM space_current_thread WHERE thread_id=NEW.id;
END;

-- A space that no longer takes part in a thread, once it is forwarded or
-- moved, no longer works for it.
CREATE TRIGGER space_changes_parties_changed AFTER UPDATE OF holder_id,space_id ON threads
BEGIN
    DELETE FROM space_current_thread WHERE thread_id=NEW.id
        AND space_id IS NOT NEW.holder_id AND space_id IS NOT NEW.space_id;
END;

-- A space that goes takes the changes no thread or post keeps, and its
-- count of them once none is kept, so a number never comes back.
CREATE TRIGGER space_changes_space_removed AFTER DELETE ON spaces BEGIN
    DELETE FROM space_changes WHERE space_id=OLD.id AND thread_id IS NULL
        AND id NOT IN (SELECT change_id FROM thread_post_changes)
        AND NOT EXISTS (SELECT 1 FROM removed_spaces WHERE space_id=OLD.id);
    DELETE FROM space_change_numbers WHERE space_id=OLD.id
        AND NOT EXISTS (SELECT 1 FROM removed_spaces WHERE space_id=OLD.id)
        AND NOT EXISTS (SELECT 1 FROM space_changes WHERE space_id=OLD.id);
END;

CREATE TRIGGER space_changes_removal_forgotten AFTER DELETE ON removed_spaces BEGIN
    DELETE FROM space_changes WHERE space_id=OLD.space_id AND thread_id IS NULL
        AND id NOT IN (SELECT change_id FROM thread_post_changes)
        AND NOT EXISTS (SELECT 1 FROM spaces WHERE id=OLD.space_id);
    DELETE FROM space_change_numbers WHERE space_id=OLD.space_id
        AND NOT EXISTS (SELECT 1 FROM spaces WHERE id=OLD.space_id)
        AND NOT EXISTS (SELECT 1 FROM space_changes WHERE space_id=OLD.space_id);
END;
