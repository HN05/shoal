-- A secret's grant binds its value in the value form, where a command gave
-- every secret before.
UPDATE access_requests
    SET record=json_set(json_remove(record,'$.specification.command'),
        '$.specification.value',
        json_object('cmd',json(json_extract(record,'$.specification.command'))))
    WHERE target_key LIKE 'secret/%'
        AND json_type(record,'$.specification.command')='array';
