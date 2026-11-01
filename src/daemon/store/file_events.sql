-- A file is added when its record is and removed when its record goes, by
-- whatever deletes it. Each event carries the record as `shoal files` lists
-- it and the space's identity, from the record itself, since a removed
-- workspace or ended chat whose files are kept has no row of its own.
CREATE TRIGGER file_added AFTER INSERT ON workspace_files BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','file_added',
        'workspace_id',CASE WHEN chat THEN NULL ELSE NEW.space_id END,
        'chat_id',CASE WHEN chat THEN NEW.space_id END,
        'repository_id',NEW.repository_id,
        'file',json_object('id',NEW.id,
            CASE WHEN chat THEN 'chat' ELSE 'workspace' END,NEW.space,
            'source',NEW.source,'name',NEW.name,'title',NEW.title,'path',NEW.path,
            'bytes',NEW.bytes,'created_at',NEW.created_at,
            'attached_by',json(NEW.attached_by))
    ) FROM (SELECT NEW.space_id IN (SELECT id FROM chats)
        OR NEW.space_id IN (SELECT chat_id FROM ended_chats) AS chat);
END;

CREATE TRIGGER file_removed AFTER DELETE ON workspace_files BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','file_removed',
        'workspace_id',CASE WHEN chat THEN NULL ELSE OLD.space_id END,
        'chat_id',CASE WHEN chat THEN OLD.space_id END,
        'repository_id',OLD.repository_id,
        'file',json_object('id',OLD.id,
            CASE WHEN chat THEN 'chat' ELSE 'workspace' END,OLD.space,
            'source',OLD.source,'name',OLD.name,'title',OLD.title,'path',OLD.path,
            'bytes',OLD.bytes,'created_at',OLD.created_at,
            'attached_by',json(OLD.attached_by))
    ) FROM (SELECT OLD.space_id IN (SELECT id FROM chats)
        OR OLD.space_id IN (SELECT chat_id FROM ended_chats) AS chat);
END;
