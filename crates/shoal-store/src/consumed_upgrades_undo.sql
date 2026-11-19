-- What tests that emulate an earlier database run first to undo
-- consumed_upgrades.sql: the upgrade state is back, with nothing in it.
CREATE TABLE upgraded_chat_agents (
    chat_id TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
    saved INTEGER NOT NULL CHECK(saved IN (0,1)),
    automatic INTEGER NOT NULL CHECK(automatic IN (0,1)),
    turn TEXT
);
CREATE TABLE chat_legacy_names (
    chat_id TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
    name TEXT NOT NULL
);
CREATE TABLE earlier_attached_files (repository_id TEXT PRIMARY KEY);
ALTER TABLE workspace_holds ADD COLUMN from_continuation INTEGER NOT NULL DEFAULT 0;
