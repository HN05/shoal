-- A removed workspace's record names the branch its restore opens, which
-- was the branch its checkpoint found checked out, or for a detached HEAD
-- one named after the short commit; a workspace at a commit keeps `at`.
UPDATE removed_spaces SET record=json_set(record,'$.branch',COALESCE(
        json_extract(record,'$.checkpoint.branch'),
        substr(json_extract(record,'$.checkpoint.head'),1,7)))
    WHERE NOT chat AND json_type(record,'$.at') IS NULL;
