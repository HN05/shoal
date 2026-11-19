-- An issue proposal names the proposing workspace's HEAD in one value,
-- which a chat's leaves out.
UPDATE access_requests SET record=json_remove(
        CASE WHEN json_type(record,'$.specification.commit') IS NULL THEN record
        ELSE json_set(record,'$.specification.workspace',json_object(
            'branch',json_extract(record,'$.specification.branch'),
            'commit',json_extract(record,'$.specification.commit')))
        END,
        '$.specification.branch','$.specification.commit')
    WHERE target_key='issue_open';
