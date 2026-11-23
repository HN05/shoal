-- The handoffs of spaces' work from one agent tool to another: the agent
-- that takes over, the session it started once its agent reports one, and
-- `record`, the session it took over from and the side it takes, as
-- `{"from": ..., "to": ...}`. A space keeps them while it or its removal
-- record lasts.
CREATE TABLE agent_handoffs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    space_id TEXT NOT NULL,
    agent TEXT NOT NULL,
    session TEXT,
    record TEXT NOT NULL
);
CREATE INDEX agent_handoffs_space ON agent_handoffs(space_id, session);

-- A handoff is journaled once the session the new tool started is known.
CREATE TRIGGER agent_handoff_started AFTER UPDATE OF session ON agent_handoffs
WHEN OLD.session IS NULL AND NEW.session IS NOT NULL BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','converted',
        '$.handoff',json_object('from',json(json_extract(NEW.record,'$.from')),
            'to',json_set(json(json_extract(NEW.record,'$.to')),'$.session',NEW.session)))
    FROM space_event_records WHERE id=NEW.space_id;
END;

CREATE TRIGGER agent_handoff_space_removed AFTER DELETE ON spaces BEGIN
    DELETE FROM agent_handoffs WHERE space_id=OLD.id
        AND NOT EXISTS (SELECT 1 FROM removed_spaces WHERE space_id=OLD.id);
END;

CREATE TRIGGER agent_handoff_removal_forgotten AFTER DELETE ON removed_spaces BEGIN
    DELETE FROM agent_handoffs WHERE space_id=OLD.space_id
        AND NOT EXISTS (SELECT 1 FROM spaces WHERE id=OLD.space_id);
END;

-- The session file a handoff's copy copies, as a `SessionCopy`.
ALTER TABLE space_files ADD COLUMN session TEXT;

DROP TRIGGER file_added;
CREATE TRIGGER file_added AFTER INSERT ON space_files BEGIN
    INSERT INTO space_events(space_id,record) SELECT NEW.space_id, json_object(
        'kind','file_added','space_id',NEW.space_id,
        'repository_id',NEW.repository_id,
        'file',json_object('id',NEW.id,
            'space',json_object('id',NEW.space_id,'name',NEW.space,
                'kind',CASE WHEN chat THEN 'chat' ELSE 'workspace' END),
            'source',NEW.source,'name',NEW.name,'title',NEW.title,'path',NEW.path,
            'bytes',NEW.bytes,'created_at',NEW.created_at,
            'attached_by',json(NEW.attached_by),'session',json(NEW.session))
    ) FROM (SELECT NEW.space_id IN (SELECT id FROM spaces WHERE kind='chat')
        OR NEW.space_id IN (SELECT space_id FROM removed_space_files WHERE chat) AS chat);
END;

DROP TRIGGER file_removed;
CREATE TRIGGER file_removed AFTER DELETE ON space_files BEGIN
    INSERT INTO space_events(space_id,record) SELECT OLD.space_id, json_object(
        'kind','file_removed','space_id',OLD.space_id,
        'repository_id',OLD.repository_id,
        'file',json_object('id',OLD.id,
            'space',json_object('id',OLD.space_id,'name',OLD.space,
                'kind',CASE WHEN chat THEN 'chat' ELSE 'workspace' END),
            'source',OLD.source,'name',OLD.name,'title',OLD.title,'path',OLD.path,
            'bytes',OLD.bytes,'created_at',OLD.created_at,
            'attached_by',json(OLD.attached_by),'session',json(OLD.session))
    ) FROM (SELECT OLD.space_id IN (SELECT id FROM spaces WHERE kind='chat')
        OR OLD.space_id IN (SELECT space_id FROM removed_space_files WHERE chat) AS chat);
END;
