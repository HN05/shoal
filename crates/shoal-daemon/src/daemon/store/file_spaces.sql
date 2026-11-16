-- A file event's file names its space, with the space's ID and kind, in
-- one value, as the file's record does, in place of the event's own; the
-- record keeps `space_id`, which the journal checks against its row.
DROP TRIGGER file_added;
DROP TRIGGER file_removed;
CREATE TRIGGER file_added AFTER INSERT ON space_files BEGIN
    INSERT INTO space_events(space_id,record) SELECT NEW.space_id, json_object(
        'kind','file_added','space_id',NEW.space_id,
        'repository_id',NEW.repository_id,
        'file',json_object('id',NEW.id,
            'space',json_object('id',NEW.space_id,'name',NEW.space,
                'kind',CASE WHEN chat THEN 'chat' ELSE 'workspace' END),
            'source',NEW.source,'name',NEW.name,'title',NEW.title,'path',NEW.path,
            'bytes',NEW.bytes,'created_at',NEW.created_at,
            'attached_by',json(NEW.attached_by))
    ) FROM (SELECT NEW.space_id IN (SELECT id FROM spaces WHERE kind='chat')
        OR NEW.space_id IN (SELECT space_id FROM removed_space_files WHERE chat) AS chat);
END;
CREATE TRIGGER file_removed AFTER DELETE ON space_files BEGIN
    INSERT INTO space_events(space_id,record) SELECT OLD.space_id, json_object(
        'kind','file_removed','space_id',OLD.space_id,
        'repository_id',OLD.repository_id,
        'file',json_object('id',OLD.id,
            'space',json_object('id',OLD.space_id,'name',OLD.space,
                'kind',CASE WHEN chat THEN 'chat' ELSE 'workspace' END),
            'source',OLD.source,'name',OLD.name,'title',OLD.title,'path',OLD.path,
            'bytes',OLD.bytes,'created_at',OLD.created_at,
            'attached_by',json(OLD.attached_by))
    ) FROM (SELECT OLD.space_id IN (SELECT id FROM spaces WHERE kind='chat')
        OR OLD.space_id IN (SELECT space_id FROM removed_space_files WHERE chat) AS chat);
END;
UPDATE space_events SET record=json_remove(json_set(record,'$.file.space',json_object(
        'id',json_extract(record,'$.space_id'),
        'name',COALESCE(json_extract(record,'$.file.workspace'),json_extract(record,'$.file.chat')),
        'kind',json_extract(record,'$.space_kind'))),
    '$.space_kind','$.file.workspace','$.file.chat')
    WHERE json_extract(record,'$.kind') IN ('file_added','file_removed');
