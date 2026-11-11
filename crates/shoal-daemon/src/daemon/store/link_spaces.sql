-- A chat links issues, PRs and CI runs as a workspace does, so the link
-- tables name the space, a workspace or chat, that links each item. IDs of
-- workspaces and chats share one namespace; the triggers below take a
-- space's links with it.
CREATE TABLE space_issue (
    space_id TEXT PRIMARY KEY,
    url TEXT NOT NULL,
    error TEXT,
    title TEXT
);
INSERT INTO space_issue(space_id,url,error,title)
    SELECT workspace_id,url,error,title FROM workspace_issue;
DROP TABLE workspace_issue;

CREATE TABLE pr_cleanup_of_spaces (
    space_id TEXT PRIMARY KEY,
    record TEXT NOT NULL
);
INSERT INTO pr_cleanup_of_spaces(space_id,record) SELECT workspace_id,record FROM pr_cleanup;
DROP TABLE pr_cleanup;
ALTER TABLE pr_cleanup_of_spaces RENAME TO pr_cleanup;

CREATE TABLE space_items (
    space_id TEXT NOT NULL,
    relation TEXT NOT NULL CHECK(relation IN ('opened','related')),
    kind TEXT NOT NULL CHECK(kind IN ('issue','pr','ci')),
    url TEXT NOT NULL,
    title TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY(space_id,relation,url)
);
INSERT INTO space_items(space_id,relation,kind,url,title,created_at)
    SELECT workspace_id,relation,kind,url,title,created_at FROM workspace_items
    ORDER BY rowid;
DROP TABLE workspace_items;

CREATE TABLE pr_activity_of_spaces (
    space_id TEXT NOT NULL,
    url TEXT NOT NULL,
    record TEXT NOT NULL,
    PRIMARY KEY(space_id,url)
);
INSERT INTO pr_activity_of_spaces(space_id,url,record)
    SELECT workspace_id,url,record FROM pr_activity;
DROP TABLE pr_activity;
ALTER TABLE pr_activity_of_spaces RENAME TO pr_activity;

CREATE TABLE space_item_activity (
    space_id TEXT NOT NULL,
    url TEXT NOT NULL,
    record TEXT NOT NULL,
    PRIMARY KEY(space_id,url)
);
INSERT INTO space_item_activity(space_id,url,record)
    SELECT workspace_id,url,record FROM workspace_item_activity;
DROP TABLE workspace_item_activity;

CREATE TRIGGER workspace_links_removed AFTER DELETE ON workspaces BEGIN
    DELETE FROM space_issue WHERE space_id=old.id;
    DELETE FROM pr_cleanup WHERE space_id=old.id;
    DELETE FROM space_items WHERE space_id=old.id;
    DELETE FROM pr_activity WHERE space_id=old.id;
    DELETE FROM space_item_activity WHERE space_id=old.id;
END;
CREATE TRIGGER chat_links_removed AFTER DELETE ON chats BEGIN
    DELETE FROM space_issue WHERE space_id=old.id;
    DELETE FROM pr_cleanup WHERE space_id=old.id;
    DELETE FROM space_items WHERE space_id=old.id;
    DELETE FROM pr_activity WHERE space_id=old.id;
    DELETE FROM space_item_activity WHERE space_id=old.id;
END;
