-- Nothing reads which space stopped an agent any longer, so neither a stop
-- Shoal decided, a chat's agent the upgrade kept nor a conversion into a
-- chat records it.
ALTER TABLE agent_stops DROP COLUMN stopped_by;
ALTER TABLE upgraded_chat_agents DROP COLUMN stopped_by;
UPDATE space_conversions SET record=json_remove(record,'$.chat.stopped_by')
    WHERE json_valid(record) AND json_type(record,'$.chat.stopped_by') IS NOT NULL;
