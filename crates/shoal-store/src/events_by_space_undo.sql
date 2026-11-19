-- What tests that emulate an earlier database run first to undo
-- events_by_space.sql: the journal is `workspace_events` again, whose
-- records name a workspace as `workspace_id` and a chat as `chat_id`, and
-- a chat's start and rename are chat events.
DROP VIEW space_refs;
DROP TRIGGER demo_started;
DROP TRIGGER demo_stopped;
DROP TRIGGER file_added;
DROP TRIGGER file_removed;
DROP TRIGGER question_answered;
DROP TRIGGER question_asked;
DROP TRIGGER simulator_acquired;
DROP TRIGGER simulator_deleted;
DROP TRIGGER simulator_released;
DROP TRIGGER space_agent_state_changed;
DROP TRIGGER space_agent_state_cleared;
DROP TRIGGER space_agent_state_set;
DROP TRIGGER space_completed;
DROP TRIGGER space_completed_again;
DROP TRIGGER space_converted;
DROP TRIGGER space_created;
DROP TRIGGER space_events_retention;
DROP TRIGGER space_leaving;
DROP TRIGGER space_parent_changed;
DROP TRIGGER space_parent_cleared;
DROP TRIGGER space_parent_set;
DROP TRIGGER space_renamed;
DROP TRIGGER space_review_cleared;
DROP TRIGGER space_review_ready;
DROP TRIGGER space_review_ready_again;
DROP TRIGGER space_undone;
DROP TRIGGER workspace_base_recorded;
DROP TRIGGER workspace_base_removed;
DROP TRIGGER workspace_branch_changed;
DROP TRIGGER workspace_branch_renamed;
DROP TRIGGER workspace_ready;
DROP TRIGGER workspace_setup_failed;

CREATE TABLE workspace_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    record TEXT NOT NULL
, space_id TEXT GENERATED ALWAYS AS (COALESCE(
    json_extract(record,'$.workspace_id'), json_extract(record,'$.chat_id'),
    json_extract(record,'$.chat.id'))) VIRTUAL, bulk INTEGER GENERATED ALWAYS AS (
    json_extract(record,'$.kind') IN ('agent_state','progress_changed')
    OR (json_extract(record,'$.kind')='item_activity'
        AND json_extract(record,'$.item_activity.update.kind')
            NOT IN ('closed','merged','reopened'))) VIRTUAL);
INSERT INTO workspace_events(id,created_at,record)
    SELECT id, created_at, CASE
        WHEN space_id IS NULL THEN record
        WHEN json_extract(record,'$.space_kind')='workspace' THEN json_remove(
            json_set(record,'$.workspace_id',space_id,'$.chat_id',NULL),
            '$.space_id','$.space_kind')
        ELSE json_remove(json_set(record,'$.workspace_id',NULL,'$.chat_id',space_id),
            '$.space_id','$.space_kind')
    END FROM space_events;
DELETE FROM sqlite_sequence WHERE name='workspace_events';
UPDATE sqlite_sequence SET name='workspace_events' WHERE name='space_events';
DROP TABLE space_events;
CREATE INDEX workspace_events_space ON workspace_events(space_id, bulk, id);

CREATE VIEW journal_spaces AS
    SELECT spaces.id,
        CASE kind WHEN 'workspace' THEN spaces.id END AS workspace_id,
        CASE kind WHEN 'chat' THEN spaces.id END AS chat_id,
        spaces.repository_id, spaces.name, workspaces.path, workspaces.branch
    FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace';
CREATE VIEW space_refs AS
    SELECT id, CASE WHEN workspace_id IS NOT NULL
            THEN json_object('id',id,'name',name,'kind','workspace','branch',branch)
            ELSE json_object('id',id,'name',name,'kind','chat') END AS ref
    FROM journal_spaces;
CREATE TRIGGER demo_started AFTER UPDATE OF has_run ON demos
WHEN NEW.has_run AND NOT OLD.has_run BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','demo_started','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'demo',json_object('name',NEW.name,'preset',json_extract(NEW.record,'$.preset'),
            'udid',json_extract(NEW.record,'$.environment.SHOAL_SIM_UDID'))
    ) FROM journal_spaces WHERE workspace_id=NEW.workspace_id;
END;
CREATE TRIGGER demo_stopped AFTER DELETE ON demos WHEN OLD.has_run BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','demo_stopped','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'demo',json_object('name',OLD.name,'preset',json_extract(OLD.record,'$.preset'),
            'udid',json_extract(OLD.record,'$.environment.SHOAL_SIM_UDID'))
    ) FROM journal_spaces WHERE workspace_id=OLD.workspace_id;
END;
CREATE TRIGGER file_added AFTER INSERT ON space_files BEGIN
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
    ) FROM (SELECT NEW.space_id IN (SELECT id FROM spaces WHERE kind='chat')
        OR NEW.space_id IN (SELECT space_id FROM removed_space_files WHERE chat) AS chat);
END;
CREATE TRIGGER file_removed AFTER DELETE ON space_files BEGIN
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
    ) FROM (SELECT OLD.space_id IN (SELECT id FROM spaces WHERE kind='chat')
        OR OLD.space_id IN (SELECT space_id FROM removed_space_files WHERE chat) AS chat);
END;
CREATE TRIGGER question_answered AFTER UPDATE OF answer ON questions
WHEN OLD.answer IS NULL AND NEW.answer IS NOT NULL BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','question_answered','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,
            'answer',NEW.answer)
    ) FROM journal_spaces WHERE id=COALESCE(NEW.workspace_id,NEW.chat_id);
END;
CREATE TRIGGER question_asked AFTER INSERT ON questions BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','question_asked','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,'answer',NULL)
    ) FROM journal_spaces WHERE id=COALESCE(NEW.workspace_id,NEW.chat_id);
END;
CREATE TRIGGER simulator_acquired AFTER UPDATE OF record ON simulators
WHEN json_extract(NEW.record,'$.state')='leased'
    AND json_extract(OLD.record,'$.state') IS NOT 'leased' BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','simulator_acquired','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'simulator',json_object('name',json_extract(NEW.record,'$.lease_name'),
            'udid',json_extract(NEW.record,'$.udid'),
            'device',json_extract(NEW.record,'$.device'),
            'runtime',json_extract(NEW.record,'$.runtime'))
    ) FROM journal_spaces WHERE workspace_id=json_extract(NEW.record,'$.workspace_id');
END;
CREATE TRIGGER simulator_deleted AFTER DELETE ON simulators
WHEN json_extract(OLD.record,'$.state')='leased' BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','simulator_released','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'simulator',json_object('name',json_extract(OLD.record,'$.lease_name'),
            'udid',json_extract(OLD.record,'$.udid'),
            'device',json_extract(OLD.record,'$.device'),
            'runtime',json_extract(OLD.record,'$.runtime'))
    ) FROM journal_spaces WHERE workspace_id=json_extract(OLD.record,'$.workspace_id');
END;
CREATE TRIGGER simulator_released AFTER UPDATE OF record ON simulators
WHEN json_extract(OLD.record,'$.state')='leased'
    AND json_extract(NEW.record,'$.state') IS NOT 'leased' BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','simulator_released','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'simulator',json_object('name',json_extract(OLD.record,'$.lease_name'),
            'udid',json_extract(OLD.record,'$.udid'),
            'device',json_extract(OLD.record,'$.device'),
            'runtime',json_extract(OLD.record,'$.runtime'))
    ) FROM journal_spaces WHERE workspace_id=json_extract(OLD.record,'$.workspace_id');
END;
CREATE TRIGGER space_agent_state_changed AFTER UPDATE OF state ON space_agent_state
WHEN OLD.state IS NOT NEW.state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,'agent_state',json_object('state',NEW.state)
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_agent_state_cleared AFTER DELETE ON space_agent_state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,'agent_state',json_object('state',NULL)
    ) FROM journal_spaces WHERE id=OLD.space_id;
END;
CREATE TRIGGER space_agent_state_set AFTER INSERT ON space_agent_state BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','agent_state','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,'agent_state',json_object('state',NEW.state)
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_completed AFTER INSERT ON space_completion BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','completed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NEW.cause,'error',NULL
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_completed_again AFTER UPDATE OF record ON space_completion
WHEN json_extract(NEW.record,'$.error') IS NULL BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','completed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NEW.cause,'error',NULL
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_converted AFTER UPDATE OF kind ON spaces
WHEN NEW.kind IS NOT OLD.kind BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','converted','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,
        'branch',branch,'cause',NULL,'error',NULL
    ) FROM journal_spaces WHERE id=NEW.id;
END;
CREATE TRIGGER space_created AFTER INSERT ON spaces BEGIN
    UPDATE spaces SET number=(SELECT number FROM free_space_number)
        WHERE id=NEW.id AND number IS NULL;
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','created','workspace_id',NEW.id,'chat_id',NULL,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'base_ref',base_ref,
        'base_workspace',json((SELECT json_object('id',base.id,'name',base.name,
                'branch',base.branch)
            FROM journal_spaces AS base WHERE base.id=workspaces.base_workspace_id)),
        'parent',json((SELECT ref FROM space_refs
            WHERE space_refs.id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id)))
    ) FROM workspaces WHERE id=NEW.id AND NEW.kind='workspace';
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','ready','workspace_id',NEW.id,'chat_id',NULL,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',path,'branch',branch,'cause',NULL,'error',NULL
    ) FROM workspaces WHERE id=NEW.id AND NEW.kind='workspace' AND state='ready';
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','chat_started','chat',json_object('id',id,'number',number,
            'repository_id',repository_id,'name',name,'agent',agent,
            'started_at',started_at)
    ) FROM spaces JOIN chats USING(id) WHERE id=NEW.id;
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_created','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'child',json((SELECT ref FROM space_refs WHERE space_refs.id=NEW.id))
    ) FROM journal_spaces
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id);
END;
CREATE TRIGGER space_leaving BEFORE DELETE ON spaces BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_removed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'child',json((SELECT ref FROM space_refs WHERE space_refs.id=OLD.id))
    ) FROM journal_spaces
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=OLD.id);
END;
CREATE TRIGGER space_parent_changed AFTER UPDATE OF parent_id ON space_parents
WHEN NEW.parent_id IS NOT OLD.parent_id BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','parent_changed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'parent',json((SELECT ref FROM space_refs WHERE space_refs.id=NEW.parent_id))
    ) FROM journal_spaces WHERE workspace_id=NEW.space_id;
END;
CREATE TRIGGER space_parent_cleared AFTER DELETE ON space_parents BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','parent_changed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,'parent',NULL
    ) FROM journal_spaces WHERE workspace_id=OLD.space_id;
END;
CREATE TRIGGER space_parent_set AFTER INSERT ON space_parents BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','parent_changed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'parent',json((SELECT ref FROM space_refs WHERE space_refs.id=NEW.parent_id))
    ) FROM journal_spaces WHERE workspace_id=NEW.space_id;
END;
CREATE TRIGGER space_renamed AFTER UPDATE OF name ON spaces
WHEN NEW.name IS NOT OLD.name BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','renamed','workspace_id',NEW.id,'chat_id',NULL,
        'repository_id',NEW.repository_id,'name',NEW.name,'path',path,
        'branch',branch,'cause',NULL,'error',NULL
    ) FROM workspaces WHERE id=NEW.id AND NEW.kind='workspace';
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','chat_renamed','chat',json_object('id',NEW.id,'repository_id',NEW.repository_id,
            'name',NEW.name,'agent',agent,'started_at',started_at)
    ) FROM chats WHERE id=NEW.id AND NEW.kind='chat';
END;
CREATE TRIGGER space_review_cleared AFTER DELETE ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_cleared','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'review',json_object('kind',OLD.kind,'url',nullif(OLD.url,''))
    ) FROM journal_spaces WHERE id=OLD.space_id;
END;
CREATE TRIGGER space_review_ready AFTER INSERT ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''))
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_review_ready_again AFTER UPDATE ON space_review BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','review_ready','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''))
    ) FROM journal_spaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_undone AFTER DELETE ON space_completion BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','undone','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL
    ) FROM journal_spaces WHERE id=OLD.space_id;
END;
CREATE TRIGGER workspace_base_recorded AFTER UPDATE OF base_ref ON workspaces
WHEN OLD.base_ref IS NULL AND NEW.base_ref IS NOT NULL BEGIN
    UPDATE workspace_events SET record=json_set(record,'$.base_ref',NEW.base_ref)
        WHERE id=(SELECT MAX(id) FROM workspace_events
            WHERE space_id=NEW.id AND json_extract(record,'$.kind')='created');
END;
CREATE TRIGGER workspace_base_removed BEFORE DELETE ON workspaces BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','base_changed','workspace_id',spaces.id,'repository_id',spaces.repository_id,
        'name',spaces.name,'path',workspaces.path,'branch',workspaces.branch,
        'cause','removed','error',NULL,
        'base_workspace',json((SELECT json_object('id',base.id,'name',base.name,
                'branch',base.branch)
            FROM journal_spaces AS base WHERE base.id=OLD.base_workspace_id))
    ) FROM spaces JOIN workspaces USING(id) WHERE workspaces.base_workspace_id=OLD.id;
    UPDATE workspaces SET base_workspace_id=OLD.base_workspace_id WHERE base_workspace_id=OLD.id;
END;
CREATE TRIGGER workspace_branch_changed AFTER UPDATE OF observed_branch ON workspaces
WHEN NEW.observed_branch IS NOT OLD.observed_branch BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','branch_changed','workspace_id',NEW.id,'repository_id',repository_id,
        'name',name,'path',NEW.path,'branch',NEW.observed_branch,'cause',NULL,'error',NULL
    ) FROM spaces WHERE id=NEW.id;
END;
CREATE TRIGGER workspace_branch_renamed AFTER UPDATE OF branch ON workspaces
WHEN NEW.branch IS NOT OLD.branch BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','renamed','workspace_id',NEW.id,'chat_id',NULL,
        'repository_id',repository_id,'name',name,'path',NEW.path,
        'branch',NEW.branch,'cause',NULL,'error',NULL
    ) FROM spaces WHERE id=NEW.id;
END;
CREATE TRIGGER workspace_events_retention AFTER INSERT ON workspace_events
WHEN NEW.bulk BEGIN
    UPDATE journal_pruned SET id=MAX(id, COALESCE((
        SELECT id FROM workspace_events WHERE space_id=NEW.space_id AND bulk
            ORDER BY id DESC LIMIT 1 OFFSET 500
    ), 0));
    DELETE FROM workspace_events WHERE space_id=NEW.space_id AND bulk AND id <= (
        SELECT id FROM workspace_events WHERE space_id=NEW.space_id AND bulk
            ORDER BY id DESC LIMIT 1 OFFSET 500
    );
END;
CREATE TRIGGER workspace_ready AFTER UPDATE OF state ON workspaces
WHEN NEW.state='ready' AND OLD.state IN ('preparing','failed','reconciling') AND NEW.error IS NULL BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','ready','workspace_id',NEW.id,'repository_id',repository_id,
        'name',name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL
    ) FROM spaces WHERE id=NEW.id;
END;
CREATE TRIGGER workspace_setup_failed AFTER UPDATE OF state ON workspaces
WHEN NEW.state='failed' AND OLD.state='preparing' BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','setup_failed','workspace_id',NEW.id,'repository_id',repository_id,
        'name',name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NEW.error
    ) FROM spaces WHERE id=NEW.id;
END;
