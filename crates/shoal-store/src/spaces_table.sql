-- Every space, a workspace or a chat, has one row in `spaces` with what
-- every space has, which its row in `workspaces` or `chats` extends with what
-- only its kind has. A conversion changes the space's kind and swaps that
-- row; deleting the space's row removes the space, with its kind's row and
-- what the tables of spaces hold for it. Its kind's row goes in first, in the
-- transaction that records it, so what journals its creation reads both.

-- The views and triggers that read what moves go, and come back once the
-- tables are rebuilt.
DROP VIEW journal_spaces;
DROP VIEW free_space_number;
DROP VIEW space_numbers;
DROP VIEW spaces;
DROP TRIGGER space_parent_set;
DROP TRIGGER space_parent_changed;
DROP TRIGGER space_parent_cleared;
DROP TRIGGER chat_child_created;
DROP TRIGGER space_agent_state_turn_kept;
DROP TRIGGER demo_started;
DROP TRIGGER demo_stopped;
DROP TRIGGER simulator_acquired;
DROP TRIGGER simulator_released;
DROP TRIGGER simulator_deleted;
DROP TRIGGER file_added;
DROP TRIGGER file_removed;

-- `repository_id` is null only for a chat without a repository.
CREATE TABLE spaces (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('workspace','chat')),
    repository_id TEXT REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    number INTEGER UNIQUE,
    mode TEXT,
    narrowing TEXT,
    CHECK(repository_id IS NOT NULL OR kind='chat')
);
INSERT INTO spaces(id,kind,repository_id,name,number,mode,narrowing)
    SELECT id,'workspace',repository_id,name,number,mode,narrowing FROM workspaces
    UNION ALL SELECT id,'chat',repository_id,name,number,mode,narrowing FROM chats;
CREATE INDEX spaces_repository ON spaces(repository_id);

CREATE TABLE workspaces_rebuilt (
    id TEXT PRIMARY KEY
        REFERENCES spaces(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    path TEXT NOT NULL UNIQUE,
    branch TEXT,
    state TEXT NOT NULL,
    error TEXT,
    base_commit TEXT,
    base_ref TEXT,
    git_dir TEXT,
    git_dir_id TEXT,
    setup_finished INTEGER NOT NULL DEFAULT 0 CHECK(setup_finished IN (0,1)),
    observed_branch TEXT,
    base_workspace_id TEXT,
    active_at INTEGER,
    start_commit TEXT,
    made_for TEXT,
    setup_skipped INTEGER NOT NULL DEFAULT 0 CHECK(setup_skipped IN (0,1)),
    disposable INTEGER NOT NULL DEFAULT 0 CHECK(disposable IN (0,1)),
    copied_from TEXT,
    copied_from_checkpoint INTEGER,
    starting_checkpoint INTEGER,
    starting_index TEXT
);
INSERT INTO workspaces_rebuilt(id,path,branch,state,error,base_commit,base_ref,git_dir,
        git_dir_id,setup_finished,observed_branch,base_workspace_id,active_at,start_commit,
        made_for,setup_skipped,disposable,copied_from,copied_from_checkpoint,
        starting_checkpoint,starting_index)
    SELECT id,path,branch,state,error,base_commit,base_ref,git_dir,
        git_dir_id,setup_finished,observed_branch,base_workspace_id,active_at,start_commit,
        made_for,setup_skipped,disposable,copied_from,copied_from_checkpoint,
        starting_checkpoint,starting_index
    FROM workspaces;
DROP TABLE workspaces;

-- `converted` marks a stopped chat a workspace became, whose restored agent
-- is told so.
CREATE TABLE chats_rebuilt (
    id TEXT PRIMARY KEY
        REFERENCES spaces(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    agent TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    origin TEXT,
    session TEXT,
    asks_in_chat INTEGER NOT NULL DEFAULT 0 CHECK(asks_in_chat IN (0,1)),
    scope_token TEXT,
    workspace_id TEXT REFERENCES workspaces(id),
    root_dir TEXT,
    converted INTEGER NOT NULL DEFAULT 0 CHECK(converted IN (0,1))
);
INSERT INTO chats_rebuilt(id,agent,started_at,origin,session,asks_in_chat,scope_token,
        workspace_id,root_dir,converted)
    SELECT id,agent,started_at,origin,session,asks_in_chat,scope_token,
        workspace_id,root_dir,converted
    FROM chats;
DROP TABLE chats;
DROP TABLE space_records;

-- Every space's attached files, a chat's as a workspace's.
DROP INDEX workspace_files_workspace;

-- The tables that keep their names are not rewritten for the renames, since
-- the triggers that name them come back below.
PRAGMA legacy_alter_table=ON;
ALTER TABLE workspaces_rebuilt RENAME TO workspaces;
ALTER TABLE chats_rebuilt RENAME TO chats;
ALTER TABLE workspace_files RENAME TO space_files;
PRAGMA legacy_alter_table=OFF;
CREATE INDEX workspaces_base ON workspaces(base_workspace_id);
CREATE UNIQUE INDEX chats_scope_token ON chats(scope_token);
CREATE INDEX chats_workspace ON chats(workspace_id);
CREATE INDEX space_files_space ON space_files(space_id, id);

-- The space numbers taken, by spaces and by the issue proposals of
-- repositories.
CREATE VIEW space_numbers AS
    SELECT number FROM spaces WHERE number IS NOT NULL
    UNION ALL SELECT space_number FROM access_requests
        WHERE workspace_id IS NULL AND chat_id IS NULL;
-- The smallest space number not taken.
CREATE VIEW free_space_number AS
    SELECT MIN(n) AS number FROM (SELECT 1 AS n UNION ALL SELECT number+1 FROM space_numbers)
        WHERE n NOT IN (SELECT number FROM space_numbers);
-- Each space as its journal entries name it.
CREATE VIEW journal_spaces AS
    SELECT spaces.id,
        CASE kind WHEN 'workspace' THEN spaces.id END AS workspace_id,
        CASE kind WHEN 'chat' THEN spaces.id END AS chat_id,
        spaces.repository_id, spaces.name, workspaces.path, workspaces.branch
    FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace';
-- Each space as the journal names it as another's parent or child.
CREATE VIEW space_refs AS
    SELECT id, CASE WHEN workspace_id IS NOT NULL
            THEN json_object('id',id,'name',name,'kind','workspace','branch',branch)
            ELSE json_object('id',id,'name',name,'kind','chat') END AS ref
    FROM journal_spaces;

-- A space takes the smallest number free when it is recorded, unless it
-- keeps one, and its creation is journaled: a workspace's with where it is
-- and, once ready, its readiness, a chat's start with its agent, and either
-- as a child of the space its parent, recorded first, is.
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

-- A space whose kind changes is journaled as converted, as the kind it
-- became.
CREATE TRIGGER space_converted AFTER UPDATE OF kind ON spaces
WHEN NEW.kind IS NOT OLD.kind BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','converted','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,
        'branch',branch,'cause',NULL,'error',NULL
    ) FROM journal_spaces WHERE id=NEW.id;
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

-- A space leaving the tree is journaled for its parent while its row and
-- its kind's are still recorded; then what the tables of spaces hold for it
-- goes.
CREATE TRIGGER space_leaving BEFORE DELETE ON spaces BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_removed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'child',json((SELECT ref FROM space_refs WHERE space_refs.id=OLD.id))
    ) FROM journal_spaces
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=OLD.id);
END;
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
    DELETE FROM space_messages WHERE space_id=OLD.id;
    DELETE FROM space_pr_titles WHERE space_id=OLD.id;
    DELETE FROM space_turn_retries WHERE space_id=OLD.id;
    DELETE FROM gate_sessions WHERE space_id=OLD.id;
    DELETE FROM gate_verdicts WHERE space_id=OLD.id;
    DELETE FROM executions WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE scope_space_id=OLD.id AND space_id IS NULL;
END;

-- What happens to a worktree is journaled with the space it belongs to.
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
CREATE TRIGGER workspace_base_recorded AFTER UPDATE OF base_ref ON workspaces
WHEN OLD.base_ref IS NULL AND NEW.base_ref IS NOT NULL BEGIN
    UPDATE workspace_events SET record=json_set(record,'$.base_ref',NEW.base_ref)
        WHERE id=(SELECT MAX(id) FROM workspace_events
            WHERE space_id=NEW.id AND json_extract(record,'$.kind')='created');
END;
-- The workspaces built on a worktree that goes build on what it built on.
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

-- A workspace's parent is journaled as it changes.
CREATE TRIGGER space_parent_set AFTER INSERT ON space_parents BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','parent_changed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'parent',json((SELECT ref FROM space_refs WHERE space_refs.id=NEW.parent_id))
    ) FROM journal_spaces WHERE workspace_id=NEW.space_id;
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

CREATE TRIGGER space_agent_state_turn_kept AFTER DELETE ON space_agent_state
WHEN OLD.space_id IN (SELECT id FROM spaces) BEGIN
    INSERT INTO space_turns(space_id,turn) VALUES (OLD.space_id,OLD.turn)
        ON CONFLICT(space_id) DO UPDATE SET turn=max(turn,excluded.turn);
END;

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

-- A file's space is a chat when the space is one, or was one once removed.
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
