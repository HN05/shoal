-- What tests that emulate an earlier database run first to undo
-- chat_removed_events.sql: a chat's removal is journaled as `chat_ended`.
CREATE TRIGGER chat_ended AFTER DELETE ON chats
WHEN OLD.id NOT IN (SELECT id FROM workspaces) BEGIN
    INSERT INTO workspace_events(record) VALUES (json_object(
        'kind','chat_ended','chat',json_object('id',OLD.id,'number',OLD.number,
            'repository_id',OLD.repository_id,'name',OLD.name,'agent',OLD.agent,
            'started_at',OLD.started_at)
    ));
END;
