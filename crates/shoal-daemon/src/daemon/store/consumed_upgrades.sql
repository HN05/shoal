-- The state that carried one upgrade each across it, which the daemon of
-- the release that made it consumed once, goes with what it kept. A hold
-- made from `shoal continue` stays as an ordinary hold, which `shoal done`
-- no longer releases.
DROP TABLE upgraded_chat_agents;
DROP TABLE chat_legacy_names;
DROP TABLE earlier_attached_files;
ALTER TABLE workspace_holds DROP COLUMN from_continuation;
