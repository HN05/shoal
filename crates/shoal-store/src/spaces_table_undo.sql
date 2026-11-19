-- What tests that emulate an earlier database run first to undo
-- spaces_table.sql: each kind's table holds what every space has again,
-- `spaces` is a view of both, and `space_records` keeps each space once.
DROP VIEW space_refs;
DROP VIEW journal_spaces;
DROP VIEW free_space_number;
DROP VIEW space_numbers;
DROP TRIGGER space_parent_set;
DROP TRIGGER space_parent_changed;
DROP TRIGGER space_parent_cleared;
DROP TRIGGER space_agent_state_turn_kept;
DROP TRIGGER demo_started;
DROP TRIGGER demo_stopped;
DROP TRIGGER simulator_acquired;
DROP TRIGGER simulator_released;
DROP TRIGGER simulator_deleted;
DROP TRIGGER file_added;
DROP TRIGGER file_removed;
DROP INDEX space_files_space;

CREATE TABLE workspaces_undone (
            id TEXT PRIMARY KEY, repository_id TEXT NOT NULL REFERENCES repositories(id),
            name TEXT NOT NULL, path TEXT NOT NULL UNIQUE, branch TEXT,
            state TEXT NOT NULL, error TEXT
        , base_commit TEXT, base_ref TEXT, git_dir TEXT, git_dir_id TEXT, setup_finished INTEGER NOT NULL DEFAULT 0 CHECK(setup_finished IN (0,1)), observed_branch TEXT, base_workspace_id TEXT, active_at INTEGER, start_commit TEXT, made_for TEXT, setup_skipped INTEGER NOT NULL DEFAULT 0 CHECK(setup_skipped IN (0,1)), disposable INTEGER NOT NULL DEFAULT 0 CHECK(disposable IN (0,1)), number INTEGER, mode TEXT, copied_from TEXT, copied_from_checkpoint INTEGER, starting_checkpoint INTEGER, starting_index TEXT, narrowing TEXT);
INSERT INTO workspaces_undone(id,repository_id,name,path,branch,state,error,base_commit,base_ref,
        git_dir,git_dir_id,setup_finished,observed_branch,base_workspace_id,active_at,start_commit,
        made_for,setup_skipped,disposable,number,mode,copied_from,copied_from_checkpoint,
        starting_checkpoint,starting_index,narrowing)
    SELECT id,repository_id,name,path,branch,state,error,base_commit,base_ref,
        git_dir,git_dir_id,setup_finished,observed_branch,base_workspace_id,active_at,start_commit,
        made_for,setup_skipped,disposable,number,mode,copied_from,copied_from_checkpoint,
        starting_checkpoint,starting_index,narrowing
    FROM spaces JOIN workspaces USING(id);
CREATE TABLE chats_undone (
    id TEXT PRIMARY KEY,
    repository_id TEXT REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    agent TEXT NOT NULL,
    started_at INTEGER NOT NULL
, origin TEXT, session TEXT, asks_in_chat INTEGER NOT NULL DEFAULT 0 CHECK(asks_in_chat IN (0,1)), scope_token TEXT, workspace_id TEXT REFERENCES workspaces(id), number INTEGER, mode TEXT NOT NULL DEFAULT 'chat', root_dir TEXT, narrowing TEXT, converted INTEGER NOT NULL DEFAULT 0 CHECK(converted IN (0,1)));
INSERT INTO chats_undone(id,repository_id,name,agent,started_at,origin,session,asks_in_chat,
        scope_token,workspace_id,number,mode,root_dir,narrowing,converted)
    SELECT id,repository_id,name,agent,started_at,origin,session,asks_in_chat,
        scope_token,workspace_id,number,COALESCE(mode,'chat'),root_dir,narrowing,converted
    FROM spaces JOIN chats USING(id);
DROP TABLE workspaces;
DROP TABLE chats;
DROP TABLE spaces;
PRAGMA legacy_alter_table=ON;
ALTER TABLE workspaces_undone RENAME TO workspaces;
ALTER TABLE chats_undone RENAME TO chats;
ALTER TABLE space_files RENAME TO workspace_files;
PRAGMA legacy_alter_table=OFF;
CREATE TABLE space_records (id TEXT PRIMARY KEY);
INSERT INTO space_records(id) SELECT id FROM workspaces UNION SELECT id FROM chats;

CREATE TRIGGER demo_started AFTER UPDATE OF has_run ON demos
WHEN NEW.has_run AND NOT OLD.has_run BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','demo_started','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'demo',json_object('name',NEW.name,'preset',json_extract(NEW.record,'$.preset'),
            'udid',json_extract(NEW.record,'$.environment.SHOAL_SIM_UDID'))
    ) FROM workspaces WHERE id=NEW.workspace_id;
END;
CREATE TRIGGER demo_stopped AFTER DELETE ON demos WHEN OLD.has_run BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','demo_stopped','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'demo',json_object('name',OLD.name,'preset',json_extract(OLD.record,'$.preset'),
            'udid',json_extract(OLD.record,'$.environment.SHOAL_SIM_UDID'))
    ) FROM workspaces WHERE id=OLD.workspace_id;
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
    ) FROM workspaces WHERE id=json_extract(NEW.record,'$.workspace_id');
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
    ) FROM workspaces WHERE id=json_extract(OLD.record,'$.workspace_id');
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
    ) FROM workspaces WHERE id=json_extract(OLD.record,'$.workspace_id');
END;
CREATE INDEX workspace_files_workspace ON workspace_files(space_id, id);
CREATE TRIGGER space_parent_set AFTER INSERT ON space_parents BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','parent_changed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'parent',json((SELECT json_object('id',id,'name',name,'kind','workspace','branch',branch)
                FROM workspaces WHERE id=NEW.parent_id
            UNION ALL SELECT json_object('id',id,'name',name,'kind','chat')
                FROM chats WHERE id=NEW.parent_id))
    ) FROM workspaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_parent_changed AFTER UPDATE OF parent_id ON space_parents
WHEN NEW.parent_id IS NOT OLD.parent_id BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','parent_changed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,
        'parent',json((SELECT json_object('id',id,'name',name,'kind','workspace','branch',branch)
                FROM workspaces WHERE id=NEW.parent_id
            UNION ALL SELECT json_object('id',id,'name',name,'kind','chat')
                FROM chats WHERE id=NEW.parent_id))
    ) FROM workspaces WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_parent_cleared AFTER DELETE ON space_parents BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','parent_changed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause',NULL,'error',NULL,'parent',NULL
    ) FROM workspaces WHERE id=OLD.space_id;
END;
CREATE TRIGGER chat_child_created AFTER INSERT ON space_parents
WHEN NEW.space_id IN (SELECT id FROM chats) BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_created','workspace_id',parent.workspace_id,'chat_id',parent.chat_id,
        'repository_id',parent.repository_id,'name',parent.name,'path',parent.path,
        'branch',parent.branch,'cause',NULL,'error',NULL,
        'child',json_object('id',child.id,'name',child.name,'kind','chat')
    ) FROM journal_spaces AS parent, chats AS child
        WHERE parent.id=NEW.parent_id AND child.id=NEW.space_id;
END;
CREATE VIEW spaces AS
    SELECT id, repository_id, name, 'workspace' AS kind, number, branch FROM workspaces
    UNION ALL SELECT id, repository_id, name, 'chat', number, NULL FROM chats;
CREATE VIEW journal_spaces AS
    SELECT spaces.id,
        CASE kind WHEN 'workspace' THEN spaces.id END AS workspace_id,
        CASE kind WHEN 'chat' THEN spaces.id END AS chat_id,
        spaces.repository_id, spaces.name, workspaces.path, spaces.branch
    FROM spaces LEFT JOIN workspaces USING(id);
CREATE INDEX workspaces_repository ON workspaces(repository_id);
CREATE TRIGGER workspace_ready AFTER UPDATE OF state ON workspaces
WHEN NEW.state='ready' AND OLD.state IN ('preparing','failed','reconciling') AND NEW.error IS NULL BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','ready','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL
    ));
END;
CREATE TRIGGER workspace_setup_failed AFTER UPDATE OF state ON workspaces
WHEN NEW.state='failed' AND OLD.state='preparing' BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','setup_failed','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NEW.error
    ));
END;
CREATE TRIGGER workspace_branch_changed AFTER UPDATE OF observed_branch ON workspaces
WHEN NEW.observed_branch IS NOT OLD.observed_branch BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','branch_changed','workspace_id',NEW.id,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.observed_branch,'cause',NULL,'error',NULL
    ));
END;
CREATE INDEX workspaces_base ON workspaces(base_workspace_id);
CREATE TRIGGER workspace_base_removed BEFORE DELETE ON workspaces BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','base_changed','workspace_id',id,'repository_id',repository_id,
        'name',name,'path',path,'branch',branch,'cause','removed','error',NULL,
        'base_workspace',json((SELECT json_object('id',id,'name',name,'branch',branch)
            FROM workspaces WHERE id=OLD.base_workspace_id))
    ) FROM workspaces WHERE base_workspace_id=OLD.id;
    UPDATE workspaces SET base_workspace_id=OLD.base_workspace_id WHERE base_workspace_id=OLD.id;
END;
CREATE UNIQUE INDEX workspaces_number ON workspaces(number);
CREATE TRIGGER workspace_numbered AFTER INSERT ON workspaces WHEN NEW.number IS NULL BEGIN
    UPDATE workspaces SET number=(SELECT number FROM free_space_number) WHERE id=NEW.id;
END;
CREATE TRIGGER workspace_base_recorded AFTER UPDATE OF base_ref ON workspaces
WHEN OLD.base_ref IS NULL AND NEW.base_ref IS NOT NULL BEGIN
    UPDATE workspace_events SET record=json_set(record,'$.base_ref',NEW.base_ref)
        WHERE id=(SELECT MAX(id) FROM workspace_events
            WHERE space_id=NEW.id AND json_extract(record,'$.kind')='created');
END;
CREATE TRIGGER workspace_created AFTER INSERT ON workspaces
WHEN NEW.id NOT IN (SELECT id FROM chats) BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','created','workspace_id',NEW.id,'chat_id',NULL,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL,
        'base_ref',NEW.base_ref,
        'base_workspace',json((SELECT json_object('id',id,'name',name,'branch',branch)
            FROM workspaces WHERE id=NEW.base_workspace_id)),
        'parent',json((SELECT json_object('id',id,'name',name,'kind','workspace','branch',branch)
                FROM workspaces WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id)
            UNION ALL SELECT json_object('id',id,'name',name,'kind','chat')
                FROM chats WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id)))
    ));
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','ready','workspace_id',NEW.id,'chat_id',NULL,'repository_id',NEW.repository_id,
        'name',NEW.name,'path',NEW.path,'branch',NEW.branch,'cause',NULL,'error',NULL
    ) WHERE NEW.state='ready';
END;
CREATE TRIGGER workspace_child_created AFTER INSERT ON workspaces
WHEN NEW.id NOT IN (SELECT id FROM chats) BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_created','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'child',json_object('id',NEW.id,'name',NEW.name,'kind','workspace','branch',NEW.branch)
    ) FROM journal_spaces
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id);
END;
CREATE TRIGGER workspace_converted AFTER INSERT ON workspaces
WHEN NEW.id IN (SELECT id FROM chats) BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','converted','workspace_id',NEW.id,'chat_id',NULL,
        'repository_id',NEW.repository_id,'name',NEW.name,'path',NEW.path,
        'branch',NEW.branch,'cause',NULL,'error',NULL
    ));
END;
CREATE UNIQUE INDEX chats_scope_token ON chats(scope_token);
CREATE INDEX chats_workspace ON chats(workspace_id);
CREATE UNIQUE INDEX chats_number ON chats(number);
CREATE TRIGGER chat_converted AFTER INSERT ON chats
WHEN NEW.id IN (SELECT id FROM workspaces) BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','converted','workspace_id',NULL,'chat_id',NEW.id,
        'repository_id',NEW.repository_id,'name',NEW.name,'path',NULL,
        'branch',NULL,'cause',NULL,'error',NULL
    ));
END;
CREATE TRIGGER workspace_name_changed AFTER UPDATE OF name, branch ON workspaces
WHEN NEW.name IS NOT OLD.name OR NEW.branch IS NOT OLD.branch BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','renamed','workspace_id',NEW.id,'chat_id',NULL,
        'repository_id',NEW.repository_id,'name',NEW.name,'path',NEW.path,
        'branch',NEW.branch,'cause',NULL,'error',NULL
    ));
END;
CREATE TRIGGER space_agent_state_turn_kept AFTER DELETE ON space_agent_state
WHEN OLD.space_id IN (SELECT id FROM workspaces UNION ALL SELECT id FROM chats) BEGIN
    INSERT INTO space_turns(space_id,turn) VALUES (OLD.space_id,OLD.turn)
        ON CONFLICT(space_id) DO UPDATE SET turn=max(turn,excluded.turn);
END;
CREATE TRIGGER chat_started AFTER INSERT ON chats
WHEN NEW.id NOT IN (SELECT id FROM workspaces) BEGIN
    UPDATE chats SET number=(SELECT number FROM free_space_number)
        WHERE id=NEW.id AND number IS NULL;
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','chat_started','chat',json_object('id',id,'number',number,
            'repository_id',repository_id,'name',name,'agent',agent,
            'started_at',started_at)
    ) FROM chats WHERE id=NEW.id;
END;
CREATE TRIGGER chat_renamed AFTER UPDATE OF name ON chats
WHEN NEW.name IS NOT OLD.name BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_renamed','chat',json_object('id',NEW.id,'repository_id',NEW.repository_id,
            'name',NEW.name,'agent',NEW.agent,'started_at',NEW.started_at)
    ));
END;
CREATE TRIGGER workspace_recorded AFTER INSERT ON workspaces BEGIN
    INSERT OR IGNORE INTO space_records(id) VALUES (NEW.id);
END;
CREATE TRIGGER chat_recorded AFTER INSERT ON chats BEGIN
    INSERT OR IGNORE INTO space_records(id) VALUES (NEW.id);
END;
CREATE TRIGGER space_removed AFTER DELETE ON space_records BEGIN
    DELETE FROM workspaces WHERE id=OLD.id;
    DELETE FROM chats WHERE id=OLD.id;
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
    DELETE FROM executions WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE space_id=OLD.id;
    DELETE FROM agent_queue WHERE scope_space_id=OLD.id AND space_id IS NULL;
END;
CREATE TRIGGER workspace_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_removed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'child',json_object('id',OLD.id,'name',OLD.name,'kind','workspace','branch',OLD.branch)
    ) FROM journal_spaces
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=OLD.id);
    DELETE FROM space_records WHERE id=OLD.id;
END;
CREATE TRIGGER chat_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    INSERT INTO workspace_events(record) SELECT json_object(
        'kind','child_removed','workspace_id',workspace_id,'chat_id',chat_id,
        'repository_id',repository_id,'name',name,'path',path,'branch',branch,
        'cause',NULL,'error',NULL,
        'child',json_object('id',OLD.id,'name',OLD.name,'kind','chat')
    ) FROM journal_spaces
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=OLD.id);
    DELETE FROM space_records WHERE id=OLD.id;
END;
CREATE TRIGGER space_messages_removed AFTER DELETE ON space_records BEGIN
    DELETE FROM space_messages WHERE space_id=OLD.id;
END;
CREATE TRIGGER workspace_turn_retry_removed AFTER DELETE ON workspaces
WHEN OLD.id NOT IN (SELECT id FROM chats) BEGIN
    DELETE FROM space_turn_retries WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_turn_retry_removed AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    DELETE FROM space_turn_retries WHERE space_id=OLD.id;
END;
CREATE TRIGGER space_pr_titles_removed AFTER DELETE ON space_records BEGIN
    DELETE FROM space_pr_titles WHERE space_id=OLD.id;
END;
CREATE TRIGGER file_added AFTER INSERT ON workspace_files BEGIN
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
    ) FROM (SELECT NEW.space_id IN (SELECT id FROM chats)
        OR NEW.space_id IN (SELECT space_id FROM removed_space_files WHERE chat) AS chat);
END;
CREATE TRIGGER file_removed AFTER DELETE ON workspace_files BEGIN
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
    ) FROM (SELECT OLD.space_id IN (SELECT id FROM chats)
        OR OLD.space_id IN (SELECT space_id FROM removed_space_files WHERE chat) AS chat);
END;
CREATE VIEW space_numbers AS
    SELECT number FROM workspaces WHERE number IS NOT NULL
    UNION ALL SELECT number FROM chats WHERE number IS NOT NULL
    UNION ALL SELECT space_number FROM access_requests
        WHERE workspace_id IS NULL AND chat_id IS NULL;
CREATE VIEW free_space_number AS
    SELECT MIN(n) AS number FROM (SELECT 1 AS n UNION ALL SELECT number+1 FROM space_numbers)
        WHERE n NOT IN (SELECT number FROM space_numbers);
CREATE TRIGGER workspace_gate_removed AFTER DELETE ON workspaces BEGIN
    DELETE FROM gate_sessions WHERE space_id=OLD.id;
    DELETE FROM gate_verdicts WHERE space_id=OLD.id;
END;
CREATE TRIGGER chat_gate_removed AFTER DELETE ON chats BEGIN
    DELETE FROM gate_sessions WHERE space_id=OLD.id;
    DELETE FROM gate_verdicts WHERE space_id=OLD.id;
END;
