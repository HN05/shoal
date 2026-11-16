-- An event names its space's kind with what only that kind has: a
-- workspace's path and branch, which a chat's events leave out. Every
-- writer of a space's events starts from the record this view builds,
-- whose kind, cause and error the event fills.
CREATE VIEW space_event_records AS
    SELECT spaces.id, spaces.kind AS space_kind, CASE spaces.kind
            WHEN 'workspace' THEN json_object('kind',NULL,'space_id',spaces.id,
                'space_kind','workspace','repository_id',spaces.repository_id,
                'name',spaces.name,'path',workspaces.path,'branch',workspaces.branch,
                'cause',NULL,'error',NULL)
            ELSE json_object('kind',NULL,'space_id',spaces.id,'space_kind','chat',
                'repository_id',spaces.repository_id,'name',spaces.name,
                'cause',NULL,'error',NULL) END AS record
    FROM spaces LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace';

UPDATE space_events SET record=json_remove(record,'$.path','$.branch')
    WHERE json_valid(record) AND json_extract(record,'$.space_kind')='chat'
        AND (json_type(record,'$.path') IS NOT NULL OR json_type(record,'$.branch') IS NOT NULL);

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
DROP TRIGGER question_asked;
DROP TRIGGER question_answered;
DROP TRIGGER space_created;
DROP TRIGGER workspace_base_removed;
DROP TRIGGER workspace_branch_changed;
DROP TRIGGER workspace_branch_renamed;
DROP TRIGGER workspace_ready;
DROP TRIGGER workspace_setup_failed;

CREATE TRIGGER space_renamed AFTER UPDATE OF name ON spaces
WHEN NEW.name IS NOT OLD.name BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,'$.kind','renamed')
        FROM space_event_records WHERE id=NEW.id;
END;
-- The event names the kind the space became, with its fields.
CREATE TRIGGER space_converted AFTER UPDATE OF kind ON spaces
WHEN NEW.kind IS NOT OLD.kind BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,'$.kind','converted')
        FROM space_event_records WHERE id=NEW.id;
END;
CREATE TRIGGER space_leaving BEFORE DELETE ON spaces BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','child_removed',
        '$.child',json((SELECT ref FROM space_refs WHERE space_refs.id=OLD.id))
    ) FROM space_event_records
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=OLD.id);
END;
CREATE TRIGGER space_parent_set AFTER INSERT ON space_parents BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','parent_changed',
        '$.parent',json((SELECT ref FROM space_refs WHERE space_refs.id=NEW.parent_id))
    ) FROM space_event_records WHERE id=NEW.space_id AND space_kind='workspace';
END;
CREATE TRIGGER space_parent_changed AFTER UPDATE OF parent_id ON space_parents
WHEN NEW.parent_id IS NOT OLD.parent_id BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','parent_changed',
        '$.parent',json((SELECT ref FROM space_refs WHERE space_refs.id=NEW.parent_id))
    ) FROM space_event_records WHERE id=NEW.space_id AND space_kind='workspace';
END;
CREATE TRIGGER space_parent_cleared AFTER DELETE ON space_parents BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','parent_changed','$.parent',NULL
    ) FROM space_event_records WHERE id=OLD.space_id AND space_kind='workspace';
END;
CREATE TRIGGER space_review_ready AFTER INSERT ON space_review BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','review_ready',
        '$.review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''))
    ) FROM space_event_records WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_review_ready_again AFTER UPDATE ON space_review BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','review_ready',
        '$.review',json_object('kind',NEW.kind,'url',nullif(NEW.url,''))
    ) FROM space_event_records WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_review_cleared AFTER DELETE ON space_review BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','review_cleared',
        '$.review',json_object('kind',OLD.kind,'url',nullif(OLD.url,''))
    ) FROM space_event_records WHERE id=OLD.space_id;
END;
CREATE TRIGGER space_agent_state_set AFTER INSERT ON space_agent_state BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','agent_state','$.agent_state',json_object('state',NEW.state)
    ) FROM space_event_records WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_agent_state_changed AFTER UPDATE OF state ON space_agent_state
WHEN OLD.state IS NOT NEW.state BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','agent_state','$.agent_state',json_object('state',NEW.state)
    ) FROM space_event_records WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_agent_state_cleared AFTER DELETE ON space_agent_state BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','agent_state','$.agent_state',json_object('state',NULL)
    ) FROM space_event_records WHERE id=OLD.space_id;
END;
CREATE TRIGGER space_completed AFTER INSERT ON space_completion BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','completed','$.cause',NEW.cause
    ) FROM space_event_records WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_completed_again AFTER UPDATE OF record ON space_completion
WHEN json_extract(NEW.record,'$.error') IS NULL BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','completed','$.cause',NEW.cause
    ) FROM space_event_records WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_undone AFTER DELETE ON space_completion BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,'$.kind','undone')
        FROM space_event_records WHERE id=OLD.space_id;
END;
CREATE TRIGGER demo_started AFTER UPDATE OF has_run ON demos
WHEN NEW.has_run AND NOT OLD.has_run BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','demo_started',
        '$.demo',json_object('name',NEW.name,'preset',json_extract(NEW.record,'$.preset'),
            'udid',json_extract(NEW.record,'$.environment.SHOAL_SIM_UDID'))
    ) FROM space_event_records WHERE id=NEW.workspace_id AND space_kind='workspace';
END;
CREATE TRIGGER demo_stopped AFTER DELETE ON demos WHEN OLD.has_run BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','demo_stopped',
        '$.demo',json_object('name',OLD.name,'preset',json_extract(OLD.record,'$.preset'),
            'udid',json_extract(OLD.record,'$.environment.SHOAL_SIM_UDID'))
    ) FROM space_event_records WHERE id=OLD.workspace_id AND space_kind='workspace';
END;
CREATE TRIGGER simulator_acquired AFTER UPDATE OF record ON simulators
WHEN json_extract(NEW.record,'$.state')='leased'
    AND json_extract(OLD.record,'$.state') IS NOT 'leased' BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','simulator_acquired',
        '$.simulator',json_object('name',json_extract(NEW.record,'$.lease_name'),
            'udid',json_extract(NEW.record,'$.udid'),
            'device',json_extract(NEW.record,'$.device'),
            'runtime',json_extract(NEW.record,'$.runtime'))
    ) FROM space_event_records
        WHERE id=json_extract(NEW.record,'$.workspace_id') AND space_kind='workspace';
END;
CREATE TRIGGER simulator_released AFTER UPDATE OF record ON simulators
WHEN json_extract(OLD.record,'$.state')='leased'
    AND json_extract(NEW.record,'$.state') IS NOT 'leased' BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','simulator_released',
        '$.simulator',json_object('name',json_extract(OLD.record,'$.lease_name'),
            'udid',json_extract(OLD.record,'$.udid'),
            'device',json_extract(OLD.record,'$.device'),
            'runtime',json_extract(OLD.record,'$.runtime'))
    ) FROM space_event_records
        WHERE id=json_extract(OLD.record,'$.workspace_id') AND space_kind='workspace';
END;
CREATE TRIGGER simulator_deleted AFTER DELETE ON simulators
WHEN json_extract(OLD.record,'$.state')='leased' BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','simulator_released',
        '$.simulator',json_object('name',json_extract(OLD.record,'$.lease_name'),
            'udid',json_extract(OLD.record,'$.udid'),
            'device',json_extract(OLD.record,'$.device'),
            'runtime',json_extract(OLD.record,'$.runtime'))
    ) FROM space_event_records
        WHERE id=json_extract(OLD.record,'$.workspace_id') AND space_kind='workspace';
END;
CREATE TRIGGER question_asked AFTER INSERT ON questions BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','question_asked',
        '$.question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,'answer',NULL)
    ) FROM space_event_records WHERE id=NEW.space_id;
END;
CREATE TRIGGER question_answered AFTER UPDATE OF answer ON questions
WHEN OLD.answer IS NULL AND NEW.answer IS NOT NULL BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','question_answered',
        '$.question',json_object('space_number',NEW.space_number,'item',NEW.number,
            'number',NEW.space_number||'.'||NEW.number,'question',NEW.question,
            'answer',NEW.answer)
    ) FROM space_event_records WHERE id=NEW.space_id;
END;
CREATE TRIGGER space_created AFTER INSERT ON spaces BEGIN
    UPDATE spaces SET number=(SELECT number FROM free_space_number)
        WHERE id=NEW.id AND number IS NULL;
    INSERT INTO space_events(space_id,record) SELECT NEW.id, json_set(record,
        '$.kind','created',
        '$.base_ref',workspaces.base_ref,
        '$.base_workspace',json((SELECT json_object('id',base.id,'name',base_space.name,
                'branch',base.branch)
            FROM workspaces AS base JOIN spaces AS base_space USING(id)
            WHERE base.id=workspaces.base_workspace_id)),
        '$.parent',json((SELECT ref FROM space_refs
            WHERE space_refs.id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id)))
    ) FROM space_event_records AS space
        LEFT JOIN workspaces ON workspaces.id=space.id AND space.space_kind='workspace'
        WHERE space.id=NEW.id;
    INSERT INTO space_events(space_id,record) SELECT NEW.id, json_set(record,'$.kind','ready')
        FROM space_event_records WHERE id=NEW.id
            AND id IN (SELECT id FROM workspaces WHERE state='ready') AND space_kind='workspace';
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','child_created',
        '$.child',json((SELECT ref FROM space_refs WHERE space_refs.id=NEW.id))
    ) FROM space_event_records
        WHERE id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id);
END;
CREATE TRIGGER workspace_base_removed BEFORE DELETE ON workspaces BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','base_changed','$.cause','removed',
        '$.base_workspace',json((SELECT json_object('id',base.id,'name',base_space.name,
                'branch',base.branch)
            FROM workspaces AS base JOIN spaces AS base_space USING(id)
            WHERE base.id=OLD.base_workspace_id))
    ) FROM space_event_records WHERE space_kind='workspace'
        AND id IN (SELECT id FROM workspaces WHERE base_workspace_id=OLD.id);
    UPDATE workspaces SET base_workspace_id=OLD.base_workspace_id WHERE base_workspace_id=OLD.id;
END;
-- A branch change names the branch Git observed checked out.
CREATE TRIGGER workspace_branch_changed AFTER UPDATE OF observed_branch ON workspaces
WHEN NEW.observed_branch IS NOT OLD.observed_branch BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','branch_changed','$.branch',NEW.observed_branch
    ) FROM space_event_records WHERE id=NEW.id AND space_kind='workspace';
END;
CREATE TRIGGER workspace_branch_renamed AFTER UPDATE OF branch ON workspaces
WHEN NEW.branch IS NOT OLD.branch BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,'$.kind','renamed')
        FROM space_event_records WHERE id=NEW.id AND space_kind='workspace';
END;
CREATE TRIGGER workspace_ready AFTER UPDATE OF state ON workspaces
WHEN NEW.state='ready' AND OLD.state IN ('preparing','failed','reconciling')
    AND NEW.error IS NULL BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,'$.kind','ready')
        FROM space_event_records WHERE id=NEW.id AND space_kind='workspace';
END;
CREATE TRIGGER workspace_setup_failed AFTER UPDATE OF state ON workspaces
WHEN NEW.state='failed' AND OLD.state='preparing' BEGIN
    INSERT INTO space_events(space_id,record) SELECT id, json_set(record,
        '$.kind','setup_failed','$.error',NEW.error
    ) FROM space_event_records WHERE id=NEW.id AND space_kind='workspace';
END;
