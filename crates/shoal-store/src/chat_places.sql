-- A chat's places are a workspace's: where its agent's execution runs, the
-- places Shoal created for it, which `owned_places` records, and the place
-- its saved session ran in. The row's copy goes, with the copies in a
-- converting chat's record and in a removed chat's.
UPDATE space_conversions SET record=json_remove(record,'$.chat.origin')
    WHERE json_extract(record,'$.to')='workspace';
UPDATE removed_spaces SET record=json_remove(record,'$.chat.origin') WHERE chat;
ALTER TABLE chats DROP COLUMN origin;
