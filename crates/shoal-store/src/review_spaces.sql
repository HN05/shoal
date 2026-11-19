-- A chat marks its linked work, or itself, ready for review as a workspace
-- does, so the marks name the space, a workspace or chat. Only a workspace's
-- marks reach the event journal, which records workspaces.
CREATE TABLE space_review (
    space_id TEXT NOT NULL,
    url TEXT NOT NULL,
    kind TEXT CHECK(kind IN ('issue','pr')),
    head TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY(space_id,url),
    CHECK((url='') = (kind IS NULL))
);
INSERT INTO space_review(space_id,url,kind,head,created_at)
    SELECT workspace_id,url,kind,head,created_at FROM workspace_review;
DROP TABLE workspace_review;

CREATE TRIGGER space_review_ready AFTER INSERT ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''),'head',NEW.head)
    ) FROM workspaces WHERE id=NEW.space_id;
END;

CREATE TRIGGER space_review_ready_again AFTER UPDATE ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''),'head',NEW.head)
    ) FROM workspaces WHERE id=NEW.space_id;
END;

-- Workspace removal deletes its marks after its row is gone, so only
-- explicit withdrawal and unlinking record an event.
CREATE TRIGGER space_review_cleared AFTER DELETE ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_cleared','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'review',json_object('kind',OLD.kind,'url',nullif(OLD.url,''),'head',OLD.head)
    ) FROM workspaces WHERE id=OLD.space_id;
END;

CREATE TRIGGER workspace_review_removed AFTER DELETE ON workspaces BEGIN
    DELETE FROM space_review WHERE space_id=old.id;
END;
CREATE TRIGGER chat_review_removed AFTER DELETE ON chats BEGIN
    DELETE FROM space_review WHERE space_id=old.id;
END;
