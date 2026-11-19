-- A queued workspace launch's plan kept the fields of a start that created
-- its workspace, with the PR it links among them. Every such launch now
-- starts in the workspace its entry records, so its plan names that
-- workspace and the PR, and nothing else of a creation. A chat's launch,
-- whose plan starts its chat, keeps its plan.
UPDATE agent_queue SET plan = json_remove(
        json_set(plan,
            '$.workspace', COALESCE(opens_workspace_id, space_id),
            '$.pr', json_extract(plan, '$.creation.pr')),
        '$.creation', '$.chat', '$.parent')
    WHERE kind = 'launch' AND json_valid(plan)
        AND json_extract(plan, '$.chat_start') IS NULL;
UPDATE agent_queue SET plan = json_remove(plan, '$.pr')
    WHERE kind = 'launch' AND json_valid(plan) AND json_type(plan, '$.pr') = 'null';
