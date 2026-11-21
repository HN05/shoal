-- The number and name of a space a thread names, as Shoal found it when it
-- named it there: its holder, a post's sender and the holder a forward
-- gave it to. A space on another machine of the hub has no row in
-- `spaces` here, so a thread reads these where it has none.
ALTER TABLE threads ADD COLUMN holder_name TEXT;
ALTER TABLE threads ADD COLUMN holder_number INTEGER;
ALTER TABLE thread_posts ADD COLUMN sender_number INTEGER;
ALTER TABLE thread_posts ADD COLUMN forwarded_name TEXT;
ALTER TABLE thread_posts ADD COLUMN forwarded_number INTEGER;

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
    ) FROM threads t JOIN space_event_records r ON r.id=t.space_id WHERE t.id=NEW.thread_id;
END;
