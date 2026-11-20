-- Alerts: one-way notices from the user, a space or Shoal, for the user or
-- a space's agent. `recipient_id` names the space an alert is for, none for
-- the user; `space` and `space_id` name the space that sent it, or else the
-- space it is about, by its name then and its ID; `subject` keys a request
-- it asks the user to decide; `attachments` lists Shoal's copies of the
-- files attached to it. For an alert to a space, `wakes` says whether it
-- starts an idle agent's next turn and `for_user` whether the user receives
-- it instead when the space goes before its agent reads it.
CREATE TABLE alerts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL,
    kind TEXT NOT NULL,
    sender TEXT NOT NULL CHECK(sender IN ('user','space','shoal')),
    recipient_id TEXT,
    space TEXT,
    space_id TEXT,
    subject TEXT,
    message TEXT NOT NULL,
    attachments TEXT NOT NULL DEFAULT '[]',
    wakes INTEGER NOT NULL DEFAULT 1 CHECK(wakes IN (0,1)),
    for_user INTEGER NOT NULL DEFAULT 0 CHECK(for_user IN (0,1)),
    read INTEGER NOT NULL DEFAULT 0 CHECK(read IN (0,1)),
    CHECK(sender <> 'space' OR space_id IS NOT NULL)
);
CREATE INDEX alerts_unread ON alerts(recipient_id, read, id);

-- Notifications become alerts to the user and keep their IDs, which a
-- following stream resumes after; a space's message to the user is the
-- space's, the rest Shoal's. Their files' records become the paths of the
-- copies.
INSERT INTO alerts(id,created_at,kind,sender,space,space_id,subject,message,attachments,read)
    SELECT n.id,n.created_at,
        CASE n.kind WHEN 'agent_message' THEN 'message' ELSE n.kind END,
        CASE WHEN n.kind='agent_message' AND n.space_id IS NOT NULL THEN 'space' ELSE 'shoal' END,
        n.workspace,n.space_id,n.subject,n.message,
        (SELECT json_group_array(path) FROM (
            SELECT f.path FROM json_each(n.files) j JOIN space_files f ON f.id=j.value
            ORDER BY j.key)),
        n.read
    FROM notifications n;

-- Space messages become alerts to their space, unread until delivered,
-- with IDs above every ID notifications gave, as are the questions whose
-- answers they carry: a space's own message passed up is that space's,
-- one about another space Shoal's, which the user receives if the space
-- goes first, and the rest the user's.
CREATE TEMP TABLE message_shift AS SELECT MAX(
    COALESCE((SELECT seq FROM sqlite_sequence WHERE name='notifications'), 0),
    COALESCE((SELECT MAX(id) FROM notifications), 0)
) AS shift;
INSERT INTO alerts(id,created_at,kind,sender,recipient_id,space,space_id,message,attachments,
        wakes,for_user)
    SELECT m.id+shift,m.created_at,
        CASE COALESCE(m.kind,'message') WHEN 'agent_message' THEN 'message'
            ELSE COALESCE(m.kind,'message') END,
        CASE WHEN m.kind='agent_message' AND m.about_id IS NOT NULL THEN 'space'
            WHEN m.about IS NULL THEN 'user' ELSE 'shoal' END,
        m.space_id,m.about,m.about_id,m.message,m.attachments,m.wakes,m.kind IS NOT NULL
    FROM space_messages m, message_shift;
UPDATE questions SET message_id=message_id+(SELECT shift FROM message_shift)
    WHERE message_id IS NOT NULL;
DELETE FROM sqlite_sequence WHERE name='alerts';
INSERT INTO sqlite_sequence(name,seq)
    SELECT 'alerts', MAX(
        (SELECT shift FROM message_shift)
            + COALESCE((SELECT seq FROM sqlite_sequence WHERE name='space_messages'), 0),
        COALESCE((SELECT MAX(id) FROM alerts), 0))
    FROM message_shift;
DROP TABLE message_shift;
DROP TABLE notifications;
DROP TABLE space_messages;
DELETE FROM sqlite_sequence WHERE name IN ('notifications','space_messages');

-- The events a space passes up to another machine name the alert's kind,
-- and whether the user receives it when no space above takes it: those
-- that named a kind for the user, which the others, ready marks and
-- decisions, did not.
UPDATE hub_outbox SET call=json_set(call,
        '$.tell.told.for_user', json(iif(json_type(call,'$.tell.told.kind') IS NULL,'false','true')),
        '$.tell.told.kind', CASE
            WHEN json_extract(call,'$.tell.told.kind')='agent_message' THEN 'message'
            WHEN json_type(call,'$.tell.told.kind') IS NOT NULL
                THEN json_extract(call,'$.tell.told.kind')
            WHEN json_type(call,'$.tell.told.about') IS NULL THEN 'approval_decided'
            ELSE 'ready_marked' END)
    WHERE json_type(call,'$.tell.told') IS NOT NULL;
UPDATE hub_queue SET call=json_set(call,
        '$.tell.told.for_user', json(iif(json_type(call,'$.tell.told.kind') IS NULL,'false','true')),
        '$.tell.told.kind', CASE
            WHEN json_extract(call,'$.tell.told.kind')='agent_message' THEN 'message'
            WHEN json_type(call,'$.tell.told.kind') IS NOT NULL
                THEN json_extract(call,'$.tell.told.kind')
            WHEN json_type(call,'$.tell.told.about') IS NULL THEN 'approval_decided'
            ELSE 'ready_marked' END)
    WHERE json_type(call,'$.tell.told') IS NOT NULL;

-- A request to message a space's agent that waits at the hub for the
-- space's machine becomes the alert to that space.
UPDATE hub_queue SET call=json_set(
        json_remove(call,'$.serve.request.method.send_agent_message'),
        '$.serve.request.method.send_alert',
        json_object(
            'to', json_object('space', json_object('space',
                json_extract(call,'$.serve.request.method.send_agent_message.space'))),
            'message', json_extract(call,'$.serve.request.method.send_agent_message.message'),
            'attachments', json(COALESCE(
                json_extract(call,'$.serve.request.method.send_agent_message.attachments'),
                '[]'))))
    WHERE json_type(call,'$.serve.request.method.send_agent_message') IS NOT NULL;

-- A space's alerts go with it; those about it stay.
DROP TRIGGER space_removed;
CREATE TRIGGER space_removed AFTER DELETE ON spaces BEGIN
    DELETE FROM space_parents WHERE space_id=OLD.id OR parent_id=OLD.id;
    DELETE FROM space_issue WHERE space_id=OLD.id;
    DELETE FROM pr_cleanup WHERE space_id=OLD.id;
    DELETE FROM space_items WHERE space_id=OLD.id;
    DELETE FROM pr_activity WHERE space_id=OLD.id;
    DELETE FROM space_item_activity WHERE space_id=OLD.id;
    DELETE FROM space_review WHERE space_id=OLD.id;
    DELETE FROM pending_updates WHERE space_id=OLD.id;
    DELETE FROM cleanup_grace WHERE space_id=OLD.id;
    DELETE FROM space_agent_state WHERE space_id=OLD.id;
    DELETE FROM space_progress WHERE space_id=OLD.id;
    DELETE FROM space_conversions WHERE space_id=OLD.id;
    DELETE FROM space_completion WHERE space_id=OLD.id;
    DELETE FROM space_turns WHERE space_id=OLD.id;
    DELETE FROM space_priority WHERE space_id=OLD.id;
    DELETE FROM space_sleep WHERE space_id=OLD.id;
    DELETE FROM alerts WHERE recipient_id=OLD.id;
    DELETE FROM space_pr_titles WHERE space_id=OLD.id;
    DELETE FROM space_turn_retries WHERE space_id=OLD.id;
    DELETE FROM gate_sessions WHERE space_id=OLD.id;
    DELETE FROM gate_verdicts WHERE space_id=OLD.id;
    DELETE FROM executions WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE scope_space_id=OLD.id AND space_id IS NULL;
END;
