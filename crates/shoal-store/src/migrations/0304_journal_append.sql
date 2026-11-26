-- Triggers detect the changes the journal records and pass the rows they
-- saw to journal_append, which builds each event from Shoal's types and
-- stores it. No trigger builds a journal record any longer.
DROP TRIGGER space_renamed;
DROP TRIGGER space_converted;
DROP TRIGGER space_leaving;
DROP TRIGGER space_parent_set;
DROP TRIGGER space_parent_changed;
DROP TRIGGER space_parent_cleared;
DROP TRIGGER space_review_ready;
DROP TRIGGER space_review_ready_again;
DROP TRIGGER space_review_cleared;
DROP TRIGGER space_agent_state_set;
DROP TRIGGER space_agent_state_changed;
DROP TRIGGER space_agent_state_cleared;
DROP TRIGGER space_completed;
DROP TRIGGER space_completed_again;
DROP TRIGGER space_undone;
DROP TRIGGER demo_started;
DROP TRIGGER demo_stopped;
DROP TRIGGER simulator_acquired;
DROP TRIGGER simulator_released;
DROP TRIGGER simulator_deleted;
DROP TRIGGER space_created;
DROP TRIGGER workspace_base_recorded;
DROP TRIGGER workspace_base_removed;
DROP TRIGGER workspace_branch_changed;
DROP TRIGGER workspace_branch_renamed;
DROP TRIGGER workspace_ready;
DROP TRIGGER workspace_setup_failed;
DROP TRIGGER thread_opened;
DROP TRIGGER thread_released;
DROP TRIGGER agent_handoff_started;
DROP TRIGGER file_added;
DROP TRIGGER file_removed;
DROP TRIGGER thread_posted;
DROP TRIGGER thread_resolved;
DROP VIEW space_event_records;
DROP VIEW space_refs;

CREATE TRIGGER space_renamed AFTER UPDATE OF name ON spaces
WHEN NEW.name IS NOT OLD.name BEGIN
    SELECT journal_append('renamed', NEW.id, json_object());
END;

CREATE TRIGGER space_converted AFTER UPDATE OF kind ON spaces
WHEN NEW.kind IS NOT OLD.kind BEGIN
    SELECT journal_append('converted', NEW.id, json_object());
END;

CREATE TRIGGER space_leaving BEFORE DELETE ON spaces BEGIN
    SELECT journal_append('child_removed', parent_id, json_object('id', OLD.id))
        FROM space_parents WHERE space_id=OLD.id;
END;

CREATE TRIGGER space_parent_set AFTER INSERT ON space_parents BEGIN
    SELECT journal_append('parent_changed', NEW.space_id,
        json_object('parent_id', NEW.parent_id));
END;

CREATE TRIGGER space_parent_changed AFTER UPDATE OF parent_id ON space_parents
WHEN NEW.parent_id IS NOT OLD.parent_id BEGIN
    SELECT journal_append('parent_changed', NEW.space_id,
        json_object('parent_id', NEW.parent_id));
END;

CREATE TRIGGER space_parent_cleared AFTER DELETE ON space_parents BEGIN
    SELECT journal_append('parent_changed', OLD.space_id, json_object('parent_id', NULL));
END;

CREATE TRIGGER space_review_ready AFTER INSERT ON space_review BEGIN
    SELECT journal_append('review_ready', NEW.space_id,
        json_object('kind', NEW.kind, 'url', NEW.url));
END;

CREATE TRIGGER space_review_ready_again AFTER UPDATE ON space_review BEGIN
    SELECT journal_append('review_ready', NEW.space_id,
        json_object('kind', NEW.kind, 'url', NEW.url));
END;

CREATE TRIGGER space_review_cleared AFTER DELETE ON space_review BEGIN
    SELECT journal_append('review_cleared', OLD.space_id,
        json_object('kind', OLD.kind, 'url', OLD.url));
END;

CREATE TRIGGER space_agent_state_set AFTER INSERT ON space_agent_state BEGIN
    SELECT journal_append('agent_state', NEW.space_id, json_object('state', NEW.state));
END;

CREATE TRIGGER space_agent_state_changed AFTER UPDATE OF state ON space_agent_state
WHEN OLD.state IS NOT NEW.state BEGIN
    SELECT journal_append('agent_state', NEW.space_id, json_object('state', NEW.state));
END;

CREATE TRIGGER space_agent_state_cleared AFTER DELETE ON space_agent_state BEGIN
    SELECT journal_append('agent_state', OLD.space_id, json_object('state', NULL));
END;

CREATE TRIGGER space_completed AFTER INSERT ON space_completion BEGIN
    SELECT journal_append('completed', NEW.space_id, json_object('cause', NEW.cause));
END;

CREATE TRIGGER space_completed_again AFTER UPDATE OF record ON space_completion
WHEN json_extract(NEW.record,'$.error') IS NULL BEGIN
    SELECT journal_append('completed', NEW.space_id, json_object('cause', NEW.cause));
END;

CREATE TRIGGER space_undone AFTER DELETE ON space_completion BEGIN
    SELECT journal_append('undone', OLD.space_id, json_object());
END;

CREATE TRIGGER demo_started AFTER UPDATE OF has_run ON demos
WHEN NEW.has_run AND NOT OLD.has_run BEGIN
    SELECT journal_append('demo_started', NEW.workspace_id,
        json_object('name', NEW.name, 'record', json(NEW.record)));
END;

CREATE TRIGGER demo_stopped AFTER DELETE ON demos WHEN OLD.has_run BEGIN
    SELECT journal_append('demo_stopped', OLD.workspace_id,
        json_object('name', OLD.name, 'record', json(OLD.record)));
END;

CREATE TRIGGER simulator_acquired AFTER UPDATE OF record ON simulators
WHEN json_extract(NEW.record,'$.state')='leased'
    AND json_extract(OLD.record,'$.state') IS NOT 'leased' BEGIN
    SELECT journal_append('simulator_acquired', json_extract(NEW.record,'$.workspace_id'),
        json_object('record', json(NEW.record)));
END;

CREATE TRIGGER simulator_released AFTER UPDATE OF record ON simulators
WHEN json_extract(OLD.record,'$.state')='leased'
    AND json_extract(NEW.record,'$.state') IS NOT 'leased' BEGIN
    SELECT journal_append('simulator_released', json_extract(OLD.record,'$.workspace_id'),
        json_object('record', json(OLD.record)));
END;

CREATE TRIGGER simulator_deleted AFTER DELETE ON simulators
WHEN json_extract(OLD.record,'$.state')='leased' BEGIN
    SELECT journal_append('simulator_released', json_extract(OLD.record,'$.workspace_id'),
        json_object('record', json(OLD.record)));
END;

CREATE TRIGGER space_created AFTER INSERT ON spaces BEGIN
    UPDATE spaces SET number=(SELECT number FROM free_space_number)
        WHERE id=NEW.id AND number IS NULL;
    SELECT journal_append('created', NEW.id, json_object());
    SELECT journal_append('ready', NEW.id, json_object())
        WHERE NEW.id IN (SELECT id FROM workspaces WHERE state='ready');
    SELECT journal_append('child_created', parent_id, json_object('id', NEW.id))
        FROM space_parents WHERE space_id=NEW.id;
END;

CREATE TRIGGER workspace_base_recorded AFTER UPDATE OF base_ref ON workspaces
WHEN OLD.base_ref IS NULL AND NEW.base_ref IS NOT NULL BEGIN
    SELECT journal_amend_created_base(NEW.id, NEW.base_ref);
END;

CREATE TRIGGER workspace_base_removed BEFORE DELETE ON workspaces BEGIN
    SELECT journal_append('base_changed', id,
        json_object('base_workspace_id', OLD.base_workspace_id))
        FROM workspaces WHERE base_workspace_id=OLD.id;
    UPDATE workspaces SET base_workspace_id=OLD.base_workspace_id WHERE base_workspace_id=OLD.id;
END;

CREATE TRIGGER workspace_branch_changed AFTER UPDATE OF observed_branch ON workspaces
WHEN NEW.observed_branch IS NOT OLD.observed_branch BEGIN
    SELECT journal_append('branch_changed', NEW.id, json_object());
END;

CREATE TRIGGER workspace_branch_renamed AFTER UPDATE OF branch ON workspaces
WHEN NEW.branch IS NOT OLD.branch BEGIN
    SELECT journal_append('renamed', NEW.id, json_object());
END;

CREATE TRIGGER workspace_ready AFTER UPDATE OF state ON workspaces
WHEN NEW.state='ready' AND OLD.state IN ('preparing','failed','reconciling')
    AND NEW.error IS NULL BEGIN
    SELECT journal_append('ready', NEW.id, json_object());
END;

CREATE TRIGGER workspace_setup_failed AFTER UPDATE OF state ON workspaces
WHEN NEW.state='failed' AND OLD.state='preparing' BEGIN
    SELECT journal_append('setup_failed', NEW.id, json_object('error', NEW.error));
END;

CREATE TRIGGER thread_opened AFTER INSERT ON threads WHEN NOT NEW.held BEGIN
    SELECT journal_append('thread_opened', NEW.space_id,
        json_object('space_number', NEW.space_number, 'number', NEW.number, 'name', NEW.name));
END;

CREATE TRIGGER thread_released AFTER UPDATE OF held ON threads
WHEN OLD.held AND NOT NEW.held BEGIN
    SELECT journal_append('thread_opened', NEW.space_id,
        json_object('space_number', NEW.space_number, 'number', NEW.number, 'name', NEW.name));
    SELECT journal_append('thread_posted', NEW.space_id,
        json_object('thread_id', thread_id, 'text', text, 'sender', sender,
            'sender_name', sender_name, 'forwarded_to', forwarded_to,
            'forwarded_name', forwarded_name))
        FROM thread_posts WHERE thread_id=NEW.id AND NOT forwards ORDER BY id;
END;

CREATE TRIGGER agent_handoff_started AFTER UPDATE OF session ON agent_handoffs
WHEN OLD.session IS NULL AND NEW.session IS NOT NULL BEGIN
    SELECT journal_append('converted', NEW.space_id,
        json_object('record', json(NEW.record), 'session', NEW.session));
END;

CREATE TRIGGER file_added AFTER INSERT ON space_files BEGIN
    SELECT journal_append('file_added', NEW.space_id,
        json_object('id', NEW.id, 'space_id', NEW.space_id, 'space', NEW.space,
            'repository_id', NEW.repository_id, 'source', NEW.source, 'name', NEW.name,
            'title', NEW.title, 'path', NEW.path, 'bytes', NEW.bytes,
            'created_at', NEW.created_at, 'attached_by', json(NEW.attached_by),
            'session', json(NEW.session)));
END;

CREATE TRIGGER file_removed AFTER DELETE ON space_files BEGIN
    SELECT journal_append('file_removed', OLD.space_id,
        json_object('id', OLD.id, 'space_id', OLD.space_id, 'space', OLD.space,
            'repository_id', OLD.repository_id, 'source', OLD.source, 'name', OLD.name,
            'title', OLD.title, 'path', OLD.path, 'bytes', OLD.bytes,
            'created_at', OLD.created_at, 'attached_by', json(OLD.attached_by),
            'session', json(OLD.session)));
END;

CREATE TRIGGER thread_posted AFTER INSERT ON thread_posts WHEN NOT NEW.forwards BEGIN
    SELECT journal_append('thread_posted', space_id,
        json_object('thread_id', NEW.thread_id, 'text', NEW.text, 'sender', NEW.sender,
            'sender_name', NEW.sender_name, 'forwarded_to', NEW.forwarded_to,
            'forwarded_name', NEW.forwarded_name))
        FROM threads WHERE id=NEW.thread_id AND NOT held;
END;

CREATE TRIGGER thread_forwarded AFTER INSERT ON thread_posts WHEN NEW.forwards BEGIN
    SELECT journal_append('thread_forwarded', space_id,
        json_object('thread_id', NEW.thread_id, 'text', NEW.text, 'sender', NEW.sender,
            'sender_name', NEW.sender_name, 'forwarded_to', NEW.forwarded_to,
            'forwarded_name', NEW.forwarded_name))
        FROM threads WHERE id=NEW.thread_id AND NOT held;
END;

CREATE TRIGGER thread_resolved AFTER UPDATE OF outcome ON threads
WHEN OLD.outcome IS NOT NEW.outcome AND NOT NEW.held BEGIN
    SELECT journal_append('thread_resolved', NEW.space_id,
        json_object('space_number', NEW.space_number, 'number', NEW.number, 'name', NEW.name,
            'outcome', json(NEW.outcome)));
END;
