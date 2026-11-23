-- What tests that emulate an earlier database run first to undo
-- agent_handoffs.sql.
DROP TABLE agent_handoffs;
DROP TRIGGER agent_handoff_space_removed;
DROP TRIGGER agent_handoff_removal_forgotten;
DROP TRIGGER file_added;
DROP TRIGGER file_removed;
ALTER TABLE space_files DROP COLUMN session;
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
