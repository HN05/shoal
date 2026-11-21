-- What one machine tells another carries a record, from which the
-- receiving machine builds its alert: the events the hub and this
-- machine's outbox hold keep their alert's kind, text and files in one.
UPDATE hub_outbox SET call=json_set(
        json_remove(call,'$.tell.told.kind','$.tell.told.message','$.tell.told.files'),
        '$.tell.told.record', json_object('alert', json_object(
            'kind', json_extract(call,'$.tell.told.kind'),
            'message', json_extract(call,'$.tell.told.message'),
            'files', json(COALESCE(json_extract(call,'$.tell.told.files'),'[]')))))
    WHERE json_type(call,'$.tell.told') IS NOT NULL;
UPDATE hub_queue SET call=json_set(
        json_remove(call,'$.tell.told.kind','$.tell.told.message','$.tell.told.files'),
        '$.tell.told.record', json_object('alert', json_object(
            'kind', json_extract(call,'$.tell.told.kind'),
            'message', json_extract(call,'$.tell.told.message'),
            'files', json(COALESCE(json_extract(call,'$.tell.told.files'),'[]')))))
    WHERE json_type(call,'$.tell.told') IS NOT NULL;
