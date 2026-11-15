-- The journal records every space's events in one shape: each names its
-- space as `space_id` with its `space_kind`, which a column beside it
-- repeats for the space's log and retention to read by; Shoal's own events
-- name none. A chat's start, rename and removal are a workspace's
-- `created`, `renamed` and `removed`, with null for what needs a worktree.

-- The views and triggers that name the journal or each kind's ID go, and
-- come back below for the new table.
DROP VIEW space_refs;
DROP VIEW journal_spaces;
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

-- `bulk` marks the events a space records often and its log leaves out:
-- its agent's turn states, its progress changes, and forge activity other
-- than an item's closing, merging and reopening.
CREATE TABLE space_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    space_id TEXT,
    record TEXT NOT NULL,
    bulk INTEGER GENERATED ALWAYS AS (
        json_extract(record,'$.kind') IN ('agent_state','progress_changed')
        OR (json_extract(record,'$.kind')='item_activity'
            AND json_extract(record,'$.item_activity.update.kind')
                NOT IN ('closed','merged','reopened'))) VIRTUAL,
    CHECK (space_id IS json_extract(record,'$.space_id'))
);

-- Each stored record keeps its ID and time. A chat's own events carried the
-- chat, with its agent on its start; every other event of a space named it
-- by its kind's ID, which also says what kind it was then.
INSERT INTO space_events(id,created_at,space_id,record)
    SELECT id, created_at, space_id, CASE
        WHEN json_extract(record,'$.kind')='chat_started' THEN json_object(
            'kind','created','space_id',space_id,'space_kind','chat',
            'repository_id',json_extract(record,'$.chat.repository_id'),
            'name',json_extract(record,'$.chat.name'),'path',NULL,'branch',NULL,
            'cause',NULL,'error',NULL,'base_ref',NULL,'base_workspace',NULL,
            'agent',json_object('name',json_extract(record,'$.chat.agent'),
                'execution_id',NULL,'exit_code',NULL))
        WHEN json_extract(record,'$.kind') IN ('chat_renamed','chat_ended') THEN json_object(
            'kind',CASE json_extract(record,'$.kind') WHEN 'chat_renamed' THEN 'renamed'
                ELSE 'removed' END,
            'space_id',space_id,'space_kind','chat',
            'repository_id',json_extract(record,'$.chat.repository_id'),
            'name',json_extract(record,'$.chat.name'),'path',NULL,'branch',NULL,
            'cause',NULL,'error',NULL)
        WHEN space_id IS NULL THEN record
        ELSE json_remove(json_set(record,'$.space_id',space_id,'$.space_kind',
                CASE WHEN json_extract(record,'$.workspace_id') IS NOT NULL
                    THEN 'workspace' ELSE 'chat' END),
            '$.workspace_id','$.chat_id')
    END FROM workspace_events;
-- New events take IDs above every one the journal gave, deleted ones too.
DELETE FROM sqlite_sequence WHERE name='space_events';
UPDATE sqlite_sequence SET name='space_events' WHERE name='workspace_events';
DROP TABLE workspace_events;
CREATE INDEX space_events_space ON space_events(space_id, bulk, id);

-- Each space keeps its newest 500 bulk events, recording the highest ID it
-- deleted in `journal_pruned`.
CREATE TRIGGER space_events_retention AFTER INSERT ON space_events
WHEN NEW.bulk BEGIN
    UPDATE journal_pruned SET id=MAX(id, COALESCE((
        SELECT id FROM space_events WHERE space_id=NEW.space_id AND bulk
            ORDER BY id DESC LIMIT 1 OFFSET 500
    ), 0));
    DELETE FROM space_events WHERE space_id=NEW.space_id AND bulk AND id <= (
        SELECT id FROM space_events WHERE space_id=NEW.space_id AND bulk
            ORDER BY id DESC LIMIT 1 OFFSET 500
    );
END;

-- Each space as the journal names it as another's parent or child.
CREATE VIEW space_refs AS
    SELECT spaces.id, CASE spaces.kind
            WHEN 'workspace' THEN json_object('id',spaces.id,'name',spaces.name,
                'kind','workspace','branch',workspaces.branch)
            ELSE json_object('id',spaces.id,'name',spaces.name,'kind','chat') END AS ref
    FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace';

-- A space takes the smallest number free when it is recorded, unless it
-- keeps one, and its creation is journaled: a workspace's with where it is
-- and, once ready, its readiness, a chat's with its agent, and either as a
-- child of the space its parent, recorded first, is.
CREATE TRIGGER space_created AFTER INSERT ON spaces BEGIN
    UPDATE spaces SET number=(SELECT number FROM free_space_number)
        WHERE id=NEW.id AND number IS NULL;
    INSERT INTO space_events(space_id,record) SELECT NEW.id, json_object(
        'kind','created','space_id',NEW.id,'space_kind',NEW.kind,
        'repository_id',NEW.repository_id,'name',NEW.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'base_ref',workspaces.base_ref,
        'base_workspace',json((SELECT json_object('id',base.id,'name',base_space.name,
                'branch',base.branch)
            FROM workspaces AS base JOIN spaces AS base_space USING(id)
            WHERE base.id=workspaces.base_workspace_id)),
        'parent',json((SELECT ref FROM space_refs
            WHERE space_refs.id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id))),
        'agent',CASE WHEN chats.id IS NOT NULL THEN json_object('name',chats.agent,
            'execution_id',NULL,'exit_code',NULL) END
    ) FROM (SELECT NEW.id AS id) AS space
        LEFT JOIN workspaces ON workspaces.id=space.id AND NEW.kind='workspace'
        LEFT JOIN chats ON chats.id=space.id AND NEW.kind='chat';
    INSERT INTO space_events(space_id,record) SELECT NEW.id, json_object(
        'kind','ready','space_id',NEW.id,'space_kind',NEW.kind,
        'repository_id',NEW.repository_id,'name',NEW.name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL
    ) FROM workspaces WHERE id=NEW.id AND NEW.kind='workspace' AND state='ready';
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','child_created','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'child',json((SELECT ref FROM space_refs WHERE space_refs.id=NEW.id))
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id);
END;

CREATE TRIGGER space_renamed AFTER UPDATE OF name ON spaces
WHEN NEW.name IS NOT OLD.name BEGIN
    INSERT INTO space_events(space_id,record) SELECT NEW.id, json_object(
        'kind','renamed','space_id',NEW.id,'space_kind',NEW.kind,
        'repository_id',NEW.repository_id,'name',NEW.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL
    ) FROM (SELECT NEW.id AS id) AS space
        LEFT JOIN workspaces ON workspaces.id=space.id AND NEW.kind='workspace';
END;

CREATE TRIGGER space_converted AFTER UPDATE OF kind ON spaces
WHEN NEW.kind IS NOT OLD.kind BEGIN
    INSERT INTO space_events(space_id,record) SELECT NEW.id, json_object(
        'kind','converted','space_id',NEW.id,'space_kind',NEW.kind,
        'repository_id',NEW.repository_id,'name',NEW.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL
    ) FROM (SELECT NEW.id AS id) AS space
        LEFT JOIN workspaces ON workspaces.id=space.id AND NEW.kind='workspace';
END;

CREATE TRIGGER space_leaving BEFORE DELETE ON spaces BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','child_removed','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'child',json((SELECT ref FROM space_refs WHERE space_refs.id=OLD.id))
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=(SELECT parent_id FROM space_parents WHERE space_id=OLD.id);
END;

-- A workspace's parent changes when a chat opens it or it returns to the
-- user.
CREATE TRIGGER space_parent_set AFTER INSERT ON space_parents BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','parent_changed','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'parent',json((SELECT ref FROM space_refs WHERE space_refs.id=NEW.parent_id))
    ) FROM spaces JOIN workspaces USING(id)
        WHERE spaces.id=NEW.space_id AND spaces.kind='workspace';
END;
CREATE TRIGGER space_parent_changed AFTER UPDATE OF parent_id ON space_parents
WHEN NEW.parent_id IS NOT OLD.parent_id BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','parent_changed','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'parent',json((SELECT ref FROM space_refs WHERE space_refs.id=NEW.parent_id))
    ) FROM spaces JOIN workspaces USING(id)
        WHERE spaces.id=NEW.space_id AND spaces.kind='workspace';
END;
CREATE TRIGGER space_parent_cleared AFTER DELETE ON space_parents BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','parent_changed','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'parent',NULL
    ) FROM spaces JOIN workspaces USING(id)
        WHERE spaces.id=OLD.space_id AND spaces.kind='workspace';
END;

-- A workspace's base ref reaches the journal with its creation, or once
-- setup records it.
CREATE TRIGGER workspace_base_recorded AFTER UPDATE OF base_ref ON workspaces
WHEN OLD.base_ref IS NULL AND NEW.base_ref IS NOT NULL BEGIN
    UPDATE space_events SET record=json_set(record,'$.base_ref',NEW.base_ref)
        WHERE id=(SELECT MAX(id) FROM space_events
            WHERE space_id=NEW.id AND json_extract(record,'$.kind')='created');
END;

-- A removed workspace's own base becomes the base of the workspaces built
-- on it.
CREATE TRIGGER workspace_base_removed BEFORE DELETE ON workspaces BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','base_changed','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause','removed','error',NULL,
        'base_workspace',json((SELECT json_object('id',base.id,'name',base_space.name,
                'branch',base.branch)
            FROM workspaces AS base JOIN spaces AS base_space USING(id)
            WHERE base.id=OLD.base_workspace_id))
    ) FROM spaces JOIN workspaces USING(id) WHERE workspaces.base_workspace_id=OLD.id;
    UPDATE workspaces SET base_workspace_id=OLD.base_workspace_id WHERE base_workspace_id=OLD.id;
END;

CREATE TRIGGER workspace_branch_changed AFTER UPDATE OF observed_branch ON workspaces
WHEN NEW.observed_branch IS NOT OLD.observed_branch BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_object(
        'kind','branch_changed','space_id',id,'space_kind',kind,
        'repository_id',repository_id,'name',name,'path',NEW.path,
        'branch',NEW.observed_branch,'cause',NULL,'error',NULL
    ) FROM spaces WHERE id=NEW.id;
END;

CREATE TRIGGER workspace_branch_renamed AFTER UPDATE OF branch ON workspaces
WHEN NEW.branch IS NOT OLD.branch BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_object(
        'kind','renamed','space_id',id,'space_kind',kind,
        'repository_id',repository_id,'name',name,'path',NEW.path,
        'branch',NEW.branch,'cause',NULL,'error',NULL
    ) FROM spaces WHERE id=NEW.id;
END;

CREATE TRIGGER workspace_ready AFTER UPDATE OF state ON workspaces
WHEN NEW.state='ready' AND OLD.state IN ('preparing','failed','reconciling')
    AND NEW.error IS NULL BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_object(
        'kind','ready','space_id',id,'space_kind',kind,
        'repository_id',repository_id,'name',name,'path',NEW.path,
        'branch',NEW.branch,'cause',NULL,'error',NULL
    ) FROM spaces WHERE id=NEW.id;
END;

CREATE TRIGGER workspace_setup_failed AFTER UPDATE OF state ON workspaces
WHEN NEW.state='failed' AND OLD.state='preparing' BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_object(
        'kind','setup_failed','space_id',id,'space_kind',kind,
        'repository_id',repository_id,'name',name,'path',NEW.path,
        'branch',NEW.branch,'cause',NULL,'error',NEW.error
    ) FROM spaces WHERE id=NEW.id;
END;

-- A space's ready marks, turn states, completion and questions.
CREATE TRIGGER space_review_ready AFTER INSERT ON space_review BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','review_ready','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''))
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=NEW.space_id;
END;
CREATE TRIGGER space_review_ready_again AFTER UPDATE ON space_review BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','review_ready','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''))
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=NEW.space_id;
END;
-- Removal deletes a space's marks after its row is gone, so only explicit
-- withdrawal and unlinking record an event.
CREATE TRIGGER space_review_cleared AFTER DELETE ON space_review BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','review_cleared','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'review',json_object('kind',OLD.kind,'url',nullif(OLD.url,''))
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=OLD.space_id;
END;

CREATE TRIGGER space_agent_state_set AFTER INSERT ON space_agent_state BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','agent_state','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'agent_state',json_object('state',NEW.state)
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=NEW.space_id;
END;
CREATE TRIGGER space_agent_state_changed AFTER UPDATE OF state ON space_agent_state
WHEN OLD.state IS NOT NEW.state BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','agent_state','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'agent_state',json_object('state',NEW.state)
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=NEW.space_id;
END;
CREATE TRIGGER space_agent_state_cleared AFTER DELETE ON space_agent_state BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','agent_state','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'agent_state',json_object('state',NULL)
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=OLD.space_id;
END;

CREATE TRIGGER space_completed AFTER INSERT ON space_completion BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','completed','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NEW.cause,'error',NULL
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=NEW.space_id;
END;
CREATE TRIGGER space_completed_again AFTER UPDATE OF record ON space_completion
WHEN json_extract(NEW.record,'$.error') IS NULL BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','completed','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NEW.cause,'error',NULL
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=NEW.space_id;
END;
CREATE TRIGGER space_undone AFTER DELETE ON space_completion BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','undone','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=OLD.space_id;
END;

CREATE TRIGGER question_asked AFTER INSERT ON questions BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','question_asked','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,'answer',NULL)
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=COALESCE(NEW.workspace_id,NEW.chat_id);
END;
CREATE TRIGGER question_answered AFTER UPDATE OF answer ON questions
WHEN OLD.answer IS NULL AND NEW.answer IS NOT NULL BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','question_answered','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,
            'answer',NEW.answer)
    ) FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
        WHERE spaces.id=COALESCE(NEW.workspace_id,NEW.chat_id);
END;

-- A workspace's demos and simulators.
CREATE TRIGGER demo_started AFTER UPDATE OF has_run ON demos
WHEN NEW.has_run AND NOT OLD.has_run BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','demo_started','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'demo',json_object('name',NEW.name,'preset',json_extract(NEW.record,'$.preset'),
            'udid',json_extract(NEW.record,'$.environment.SHOAL_SIM_UDID'))
    ) FROM spaces JOIN workspaces USING(id)
        WHERE spaces.id=NEW.workspace_id AND spaces.kind='workspace';
END;
CREATE TRIGGER demo_stopped AFTER DELETE ON demos WHEN OLD.has_run BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','demo_stopped','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'demo',json_object('name',OLD.name,'preset',json_extract(OLD.record,'$.preset'),
            'udid',json_extract(OLD.record,'$.environment.SHOAL_SIM_UDID'))
    ) FROM spaces JOIN workspaces USING(id)
        WHERE spaces.id=OLD.workspace_id AND spaces.kind='workspace';
END;

CREATE TRIGGER simulator_acquired AFTER UPDATE OF record ON simulators
WHEN json_extract(NEW.record,'$.state')='leased'
    AND json_extract(OLD.record,'$.state') IS NOT 'leased' BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','simulator_acquired','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'simulator',json_object('name',json_extract(NEW.record,'$.lease_name'),
            'udid',json_extract(NEW.record,'$.udid'),
            'device',json_extract(NEW.record,'$.device'),
            'runtime',json_extract(NEW.record,'$.runtime'))
    ) FROM spaces JOIN workspaces USING(id)
        WHERE spaces.id=json_extract(NEW.record,'$.workspace_id') AND spaces.kind='workspace';
END;
CREATE TRIGGER simulator_released AFTER UPDATE OF record ON simulators
WHEN json_extract(OLD.record,'$.state')='leased'
    AND json_extract(NEW.record,'$.state') IS NOT 'leased' BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','simulator_released','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'simulator',json_object('name',json_extract(OLD.record,'$.lease_name'),
            'udid',json_extract(OLD.record,'$.udid'),
            'device',json_extract(OLD.record,'$.device'),
            'runtime',json_extract(OLD.record,'$.runtime'))
    ) FROM spaces JOIN workspaces USING(id)
        WHERE spaces.id=json_extract(OLD.record,'$.workspace_id') AND spaces.kind='workspace';
END;
CREATE TRIGGER simulator_deleted AFTER DELETE ON simulators
WHEN json_extract(OLD.record,'$.state')='leased' BEGIN
    INSERT INTO space_events(space_id,record) SELECT spaces.id, json_object(
        'kind','simulator_released','space_id',spaces.id,'space_kind',spaces.kind,
        'repository_id',spaces.repository_id,'name',spaces.name,
        'path',workspaces.path,'branch',workspaces.branch,'cause',NULL,'error',NULL,
        'simulator',json_object('name',json_extract(OLD.record,'$.lease_name'),
            'udid',json_extract(OLD.record,'$.udid'),
            'device',json_extract(OLD.record,'$.device'),
            'runtime',json_extract(OLD.record,'$.runtime'))
    ) FROM spaces JOIN workspaces USING(id)
        WHERE spaces.id=json_extract(OLD.record,'$.workspace_id') AND spaces.kind='workspace';
END;

-- A file's space may be gone, so its kind comes from the removal record of
-- its files.
CREATE TRIGGER file_added AFTER INSERT ON space_files BEGIN
    INSERT INTO space_events(space_id,record) SELECT NEW.space_id, json_object(
        'kind','file_added','space_id',NEW.space_id,
        'space_kind',CASE WHEN chat THEN 'chat' ELSE 'workspace' END,
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
    INSERT INTO space_events(space_id,record) SELECT OLD.space_id, json_object(
        'kind','file_removed','space_id',OLD.space_id,
        'space_kind',CASE WHEN chat THEN 'chat' ELSE 'workspace' END,
        'repository_id',OLD.repository_id,
        'file',json_object('id',OLD.id,
            CASE WHEN chat THEN 'chat' ELSE 'workspace' END,OLD.space,
            'source',OLD.source,'name',OLD.name,'title',OLD.title,'path',OLD.path,
            'bytes',OLD.bytes,'created_at',OLD.created_at,
            'attached_by',json(OLD.attached_by))
    ) FROM (SELECT OLD.space_id IN (SELECT id FROM spaces WHERE kind='chat')
        OR OLD.space_id IN (SELECT space_id FROM removed_space_files WHERE chat) AS chat);
END;
