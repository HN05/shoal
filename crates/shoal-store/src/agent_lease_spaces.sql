-- An agent's lease names the space whose agent acquired it, which keeps it
-- across its sleeps, restores and restarts, where it named the execution
-- that ran the agent; a demo's still names its execution. A lease whose
-- execution no longer exists names its own workspace, whose agent the
-- daemon releases it with once that agent has ended.
ALTER TABLE resource_leases RENAME COLUMN acquirer_execution TO acquirer_id;
UPDATE resource_leases SET acquirer_id=COALESCE(
        (SELECT space_id FROM executions WHERE id=resource_leases.acquirer_id), workspace_id)
    WHERE acquirer='agent';

ALTER TABLE container_projects RENAME COLUMN acquirer_execution TO acquirer_id;
UPDATE container_projects SET acquirer_id=COALESCE(
        (SELECT space_id FROM executions WHERE id=container_projects.acquirer_id), workspace_id)
    WHERE acquirer='agent';

UPDATE simulators SET record=json_set(json_remove(record,'$.acquirer.execution_id'),
        '$.acquirer.space_id',COALESCE(
            (SELECT space_id FROM executions
                WHERE id=json_extract(simulators.record,'$.acquirer.execution_id')),
            json_extract(record,'$.workspace_id')))
    WHERE json_valid(record) AND json_extract(record,'$.acquirer.kind')='agent';
