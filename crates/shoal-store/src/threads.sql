-- Threads replace questions: conversations between whoever opened one, the
-- user, a space or Shoal, and whoever holds it now, with posts from both
-- sides and a typed resolution. A thread is numbered in `space_id`: its
-- opener's space, or the space the user or Shoal opened it with. Its
-- number goes with that space, and the thread with the space's removal
-- record. `holder_id` names the space that holds it, none for the user;
-- `outcome` holds its resolution as JSON, by `resolved_by`, with the space
-- that resolved it, or whose completion or removal closed it, in
-- `resolver_id`.
CREATE TABLE threads (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    space_id TEXT NOT NULL,
    space_number INTEGER,
    number INTEGER NOT NULL,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    opener TEXT NOT NULL CHECK(opener IN ('user','space','shoal')),
    holder_id TEXT,
    outcome TEXT,
    resolved_by TEXT CHECK(resolved_by IN ('user','space','shoal')),
    resolver_id TEXT,
    resolved_at INTEGER,
    CHECK((outcome IS NULL) = (resolved_by IS NULL)
        AND (outcome IS NULL) = (resolved_at IS NULL))
);
CREATE UNIQUE INDEX threads_number ON threads(space_number, number);
CREATE INDEX threads_space ON threads(space_id, id);
CREATE INDEX threads_holder ON threads(holder_id);

-- A post's `side` is its opener's or its holder's; `sender_id` and
-- `sender_name` name the space that sent it. A post that `forwards` the
-- thread names its new holder in `forwarded_to`, none for the user, with
-- its note as its text. `files` lists the records of the files sent with
-- it, and `alert_id` the alert that carried it to whom it was for.
CREATE TABLE thread_posts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    side TEXT NOT NULL CHECK(side IN ('opener','holder')),
    sender TEXT NOT NULL CHECK(sender IN ('user','space','shoal')),
    sender_id TEXT,
    sender_name TEXT,
    text TEXT NOT NULL,
    choices TEXT NOT NULL DEFAULT '[]',
    recommendation TEXT,
    reason TEXT,
    choice INTEGER,
    files TEXT NOT NULL DEFAULT '[]',
    in_chat INTEGER NOT NULL DEFAULT 0 CHECK(in_chat IN (0,1)),
    forwards INTEGER NOT NULL DEFAULT 0 CHECK(forwards IN (0,1)),
    forwarded_to TEXT,
    alert_id INTEGER
);
CREATE INDEX thread_posts_thread ON thread_posts(thread_id, id);

-- Every space that held a thread, which reads it from then on.
CREATE TABLE thread_holders (
    thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    space_id TEXT NOT NULL,
    PRIMARY KEY(thread_id, space_id)
);

-- The threads a space linked, whose new posts reach its agent.
CREATE TABLE thread_links (
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    PRIMARY KEY(space_id, thread_id)
);

-- A question went to its coordinator: the nearest space above with an
-- agent, a chat or a workspace with an agent's execution recorded, else
-- to the user.
CREATE TEMP TABLE question_holders AS
    WITH RECURSIVE above(question, space_id, depth) AS (
        SELECT q.id, p.parent_id, 1 FROM questions q JOIN space_parents p ON p.space_id=q.space_id
        UNION ALL
        SELECT a.question, p.parent_id, a.depth+1
            FROM above a JOIN space_parents p ON p.space_id=a.space_id)
    SELECT question, space_id FROM (
        SELECT question, space_id,
            ROW_NUMBER() OVER (PARTITION BY question ORDER BY depth) AS nearest
        FROM above
        WHERE EXISTS (SELECT 1 FROM spaces WHERE id=above.space_id AND kind='chat')
            OR EXISTS (SELECT 1 FROM executions
                WHERE space_id=above.space_id AND agent IS NOT NULL))
    WHERE nearest=1;

-- Each question becomes a question thread under its ID and number, opened
-- by its space and held by its coordinator. Its first post keeps what it asked, and an answer not yet
-- delivered follows as the user's post, its alert still carrying it; a
-- closed question is closed by its space's completion, a withdrawn one
-- withdrawn by its space.
INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener,holder_id,
        outcome,resolved_by,resolver_id,resolved_at)
    SELECT q.id,q.space_id,q.space_number,q.number,'question',
        CASE WHEN q.in_chat THEN 'Asked in the chat' ELSE q.question END,
        q.created_at,'space',(SELECT space_id FROM question_holders WHERE question=q.id),
        CASE WHEN q.answer IS NOT NULL OR q.closed_at IS NULL THEN NULL
            WHEN q.withdrawn THEN json_object('outcome','withdrawn')
            ELSE json_object('outcome','closed','cause','completion') END,
        CASE WHEN q.answer IS NOT NULL OR q.closed_at IS NULL THEN NULL
            WHEN q.withdrawn THEN 'space' ELSE 'shoal' END,
        CASE WHEN q.answer IS NOT NULL OR q.closed_at IS NULL THEN NULL ELSE q.space_id END,
        CASE WHEN q.answer IS NOT NULL THEN NULL ELSE q.closed_at END
    FROM questions q;
INSERT INTO thread_holders(thread_id,space_id)
    SELECT id,holder_id FROM threads WHERE holder_id IS NOT NULL;
INSERT INTO thread_posts(thread_id,created_at,side,sender,sender_id,sender_name,text,choices,
        recommendation,reason,files,in_chat)
    SELECT q.id,q.created_at,'opener','space',q.space_id,
        (SELECT name FROM spaces WHERE id=q.space_id),q.question,q.choices,
        NULLIF(q.recommendation,''),q.reason,q.files,q.in_chat
    FROM questions q ORDER BY q.id;
INSERT INTO thread_posts(thread_id,created_at,side,sender,text,alert_id)
    SELECT q.id,q.answered_at,'holder','user',q.answer,q.message_id
    FROM questions q WHERE q.answer IS NOT NULL ORDER BY q.id;
DELETE FROM sqlite_sequence WHERE name='threads';
INSERT INTO sqlite_sequence(name,seq)
    SELECT 'threads', MAX(COALESCE((SELECT seq FROM sqlite_sequence WHERE name='questions'), 0),
        COALESCE((SELECT MAX(id) FROM threads), 0));
DROP TABLE question_holders;
DROP TRIGGER question_asked;
DROP TRIGGER question_answered;
DROP TABLE questions;
DELETE FROM sqlite_sequence WHERE name='questions';

-- The files sent with questions were sent with their posts.
UPDATE space_files SET source='thread' WHERE source='question';

-- Alerts about questions become alerts about threads.
UPDATE alerts SET kind=CASE kind WHEN 'question_withdrawn' THEN 'thread_resolved'
        ELSE 'thread_posted' END
    WHERE kind IN ('question_asked','question_withdrawn','question_answered');
UPDATE hub_outbox SET call=json_set(call,'$.tell.told.kind',
        CASE json_extract(call,'$.tell.told.kind') WHEN 'question_withdrawn' THEN 'thread_resolved'
            ELSE 'thread_posted' END)
    WHERE json_extract(call,'$.tell.told.kind') IN ('question_asked','question_withdrawn');
UPDATE hub_queue SET call=json_set(call,'$.tell.told.kind',
        CASE json_extract(call,'$.tell.told.kind') WHEN 'question_withdrawn' THEN 'thread_resolved'
            ELSE 'thread_posted' END)
    WHERE json_extract(call,'$.tell.told.kind') IN ('question_asked','question_withdrawn');

-- An answer that waits at the hub for the asking space's machine becomes
-- the user's post in its thread; an accepted recommendation posts the
-- recommendation the user was shown, and one without it is dropped.
UPDATE hub_queue SET call=json_set(
        json_remove(call,'$.serve.request.method.answer_question'),
        '$.serve.request.method.post',
        json_object(
            'thread', json(json_extract(call,'$.serve.request.method.answer_question.number')),
            'post', json_object('text',
                json_extract(call,'$.serve.request.method.answer_question.answer'))))
    WHERE json_type(call,'$.serve.request.method.answer_question') IS NOT NULL;
DELETE FROM hub_queue
    WHERE json_type(call,'$.serve.request.method.accept_question_recommendation.shown') IS NOT 'object'
        AND json_type(call,'$.serve.request.method.accept_question_recommendation') IS NOT NULL;
UPDATE hub_queue SET call=json_set(
        json_remove(call,'$.serve.request.method.accept_question_recommendation'),
        '$.serve.request.method.post',
        json_object(
            'thread', json(json_extract(call,
                '$.serve.request.method.accept_question_recommendation.number')),
            'post', json_object('text', json_extract(call,
                '$.serve.request.method.accept_question_recommendation.shown.recommendation'))))
    WHERE json_type(call,'$.serve.request.method.accept_question_recommendation') IS NOT NULL;
ALTER TABLE hub_escalated_questions RENAME TO hub_escalated_threads;

-- A space's log names its questions' events as its threads' posts.
UPDATE space_events SET record=json_set(json_remove(record,'$.question'),
        '$.kind','thread_posted',
        '$.thread',json_object(
            'space_number',json_extract(record,'$.question.space_number'),
            'item',json_extract(record,'$.question.item'),
            'number',json_extract(record,'$.question.number'),
            'name',json_extract(record,'$.question.question'),
            'text',COALESCE(json_extract(record,'$.question.answer'),
                json_extract(record,'$.question.question'))))
    WHERE json_extract(record,'$.kind') IN ('question_asked','question_answered');
UPDATE space_events SET record=json_set(record,'$.threads',json(json_extract(record,'$.questions')))
    WHERE json_type(record,'$.questions') IS NOT NULL;
UPDATE space_events SET record=json_remove(record,'$.questions')
    WHERE json_type(record,'$.questions') IS NOT NULL;

-- A turn that asked in the chat names the thread its final message posts
-- in; no thread was named before.
ALTER TABLE executions DROP COLUMN asks_in_chat;
ALTER TABLE executions ADD COLUMN chat_thread INTEGER REFERENCES threads(id) ON DELETE SET NULL;

CREATE TRIGGER thread_opened AFTER INSERT ON threads BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','thread_opened',
        '$.thread',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'name',NEW.name)
    ) FROM space_event_records WHERE id=NEW.space_id;
END;

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
                    NEW.forwarded_to) END)
    ) FROM threads t JOIN space_event_records r ON r.id=t.space_id WHERE t.id=NEW.thread_id;
END;

CREATE TRIGGER thread_resolved AFTER UPDATE OF outcome ON threads
WHEN OLD.outcome IS NOT NEW.outcome BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','thread_resolved',
        '$.thread',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'name',NEW.name,
            'outcome',json_extract(NEW.outcome,'$.outcome'))
    ) FROM space_event_records WHERE id=NEW.space_id;
END;

-- A thread's number follows its space's, and goes with the space; the
-- threads of a space without a removal record to restore it go too.
CREATE TRIGGER thread_space_numbered AFTER UPDATE OF number ON spaces
WHEN OLD.number IS NOT NEW.number BEGIN
    UPDATE threads SET space_number=NEW.number WHERE space_id=NEW.id;
END;

CREATE TRIGGER thread_space_restored AFTER INSERT ON spaces
WHEN NEW.number IS NOT NULL BEGIN
    UPDATE threads SET space_number=NEW.number WHERE space_id=NEW.id;
END;

CREATE TRIGGER thread_space_removed AFTER DELETE ON spaces BEGIN
    UPDATE threads SET space_number=NULL WHERE space_id=OLD.id;
    DELETE FROM threads WHERE space_id=OLD.id
        AND NOT EXISTS (SELECT 1 FROM removed_spaces WHERE space_id=OLD.id);
END;

CREATE TRIGGER thread_removal_forgotten AFTER DELETE ON removed_spaces BEGIN
    DELETE FROM threads WHERE space_id=OLD.space_id
        AND NOT EXISTS (SELECT 1 FROM spaces WHERE id=OLD.space_id);
END;
