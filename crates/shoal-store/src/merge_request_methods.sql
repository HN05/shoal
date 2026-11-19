-- A merge decision binds the method it merges by, which every merge
-- requested before took: a fast-forward or merge commit.
UPDATE access_requests
    SET record=json_set(record,'$.specification.method','merge')
    WHERE target_key='merge' AND json_type(record,'$.specification.method') IS NULL;
