-- A space's agent is the one its execution runs, else the one it saved,
-- else the one its queued launch starts, whatever its kind: a chat's row
-- no longer names it, nor marks a chat a workspace became, which nothing
-- read. A chat's creation names no agent, as a workspace's does not; its
-- agent's start names it. The copies in a converting chat's record and in a
-- removed chat's go too.
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
            WHERE space_refs.id=(SELECT parent_id FROM space_parents WHERE space_id=NEW.id)))
    ) FROM (SELECT NEW.id AS id) AS space
        LEFT JOIN workspaces ON workspaces.id=space.id AND NEW.kind='workspace';
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
UPDATE space_conversions SET record=json_remove(record,'$.chat.agent','$.chat.converted')
    WHERE json_extract(record,'$.to')='workspace';
UPDATE removed_spaces SET record=json_remove(record,'$.chat.agent') WHERE chat;
ALTER TABLE chats DROP COLUMN agent;
ALTER TABLE chats DROP COLUMN converted;
