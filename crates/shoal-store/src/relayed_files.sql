-- An alert another machine relays here records the files its space sent
-- there, which that machine keeps, beside the paths of those this machine
-- keeps. The events the hub and this machine's outbox hold named such files
-- by name only, which their text now says; `relayed_files.rs` rewrites the
-- files of the summaries the hub keeps.
ALTER TABLE alerts ADD COLUMN relayed_files TEXT NOT NULL DEFAULT '[]';
UPDATE hub_outbox SET call=json_remove(json_set(call,
        '$.tell.told.record.alert.message',
        json_extract(call, '$.tell.told.record.alert.message') || char(10)
            || 'Files it sent, which stay on its machine: '
            || (SELECT group_concat(value, ', ')
                FROM json_each(call, '$.tell.told.record.alert.files'))),
    '$.tell.told.record.alert.files')
    WHERE json_array_length(call, '$.tell.told.record.alert.files') > 0;
UPDATE hub_outbox SET call=json_remove(json_set(call,
        '$.tell.told.record.thread.agent',
        json_extract(call, '$.tell.told.record.thread.agent') || char(10)
            || 'Files it sent, which stay on its machine: '
            || (SELECT group_concat(value, ', ')
                FROM json_each(call, '$.tell.told.record.thread.files')),
        '$.tell.told.record.thread.user',
        json_extract(call, '$.tell.told.record.thread.user') || char(10)
            || 'Files it sent, which stay on its machine: '
            || (SELECT group_concat(value, ', ')
                FROM json_each(call, '$.tell.told.record.thread.files'))),
    '$.tell.told.record.thread.files')
    WHERE json_array_length(call, '$.tell.told.record.thread.files') > 0;
UPDATE hub_queue SET call=json_remove(json_set(call,
        '$.tell.told.record.alert.message',
        json_extract(call, '$.tell.told.record.alert.message') || char(10)
            || 'Files it sent, which stay on its machine: '
            || (SELECT group_concat(value, ', ')
                FROM json_each(call, '$.tell.told.record.alert.files'))),
    '$.tell.told.record.alert.files')
    WHERE json_array_length(call, '$.tell.told.record.alert.files') > 0;
UPDATE hub_queue SET call=json_remove(json_set(call,
        '$.tell.told.record.thread.agent',
        json_extract(call, '$.tell.told.record.thread.agent') || char(10)
            || 'Files it sent, which stay on its machine: '
            || (SELECT group_concat(value, ', ')
                FROM json_each(call, '$.tell.told.record.thread.files')),
        '$.tell.told.record.thread.user',
        json_extract(call, '$.tell.told.record.thread.user') || char(10)
            || 'Files it sent, which stay on its machine: '
            || (SELECT group_concat(value, ', ')
                FROM json_each(call, '$.tell.told.record.thread.files'))),
    '$.tell.told.record.thread.files')
    WHERE json_array_length(call, '$.tell.told.record.thread.files') > 0;
