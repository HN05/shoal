-- A thread that holds its posts, a pending review's comment, records no
-- event while it holds them, since the space it is numbered in may be the
-- reviewed workspace, whose agent reads its log; releasing it records its
-- opening and each of its posts then.
DROP TRIGGER thread_opened;
CREATE TRIGGER thread_opened AFTER INSERT ON threads WHEN NOT NEW.held BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','thread_opened',
        '$.thread',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'name',NEW.name)
    ) FROM space_event_records WHERE id=NEW.space_id;
END;

DROP TRIGGER thread_resolved;
CREATE TRIGGER thread_resolved AFTER UPDATE OF outcome ON threads
WHEN OLD.outcome IS NOT NEW.outcome AND NOT NEW.held BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','thread_resolved',
        '$.thread',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'name',NEW.name,
            'outcome',json_extract(NEW.outcome,'$.outcome'))
    ) FROM space_event_records WHERE id=NEW.space_id;
END;

DROP TRIGGER thread_posted;
CREATE TRIGGER thread_posted AFTER INSERT ON thread_posts BEGIN
    INSERT INTO space_events(space_id,record) SELECT r.id, json_set(r.record,
        '$.kind',iif(NEW.forwards,'thread_forwarded','thread_posted'),
        '$.thread',json_object('space_number',t.space_number,'item',t.number,
            'number',t.space_number||'.'||t.number,'name',t.name,'text',NEW.text,
            'sender',COALESCE(NEW.sender_name,
                CASE NEW.sender WHEN 'user' THEN 'the user' ELSE 'Shoal' END),
            'to',CASE WHEN NOT NEW.forwards THEN NULL
                WHEN NEW.forwarded_to IS NULL THEN 'the user'
                ELSE COALESCE((SELECT name FROM spaces WHERE id=NEW.forwarded_to),
                    NEW.forwarded_name, NEW.forwarded_to) END)
    ) FROM threads t JOIN space_event_records r ON r.id=t.space_id
    WHERE t.id=NEW.thread_id AND NOT t.held;
END;

CREATE TRIGGER thread_released AFTER UPDATE OF held ON threads
WHEN OLD.held AND NOT NEW.held BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','thread_opened',
        '$.thread',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'name',NEW.name)
    ) FROM space_event_records WHERE id=NEW.space_id;
    INSERT INTO space_events(space_id,record) SELECT r.id, json_set(r.record,
        '$.kind','thread_posted',
        '$.thread',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'name',NEW.name,'text',p.text,
            'sender',COALESCE(p.sender_name,
                CASE p.sender WHEN 'user' THEN 'the user' ELSE 'Shoal' END))
    ) FROM thread_posts p JOIN space_event_records r ON r.id=NEW.space_id
    WHERE p.thread_id=NEW.id AND NOT p.forwards ORDER BY p.id;
END;
