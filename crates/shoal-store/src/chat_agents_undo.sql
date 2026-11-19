-- What tests that emulate an earlier database run first to undo
-- chat_agents.sql: a chat's row names its agent, and a chat's creation names
-- it.
ALTER TABLE chats ADD COLUMN agent TEXT NOT NULL DEFAULT 'claude';
ALTER TABLE chats ADD COLUMN converted INTEGER NOT NULL DEFAULT 0 CHECK(converted IN (0,1));
DROP TRIGGER space_created;
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
