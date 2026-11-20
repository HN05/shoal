-- What tests that emulate an earlier database run first to undo
-- space_changes.sql: Shoal records no changes.
DROP TRIGGER space_changes_resolved;
DROP TRIGGER space_changes_parties_changed;
DROP TRIGGER space_changes_space_removed;
DROP TRIGGER space_changes_removal_forgotten;
DROP TABLE thread_post_changes;
DROP TABLE space_current_thread;
DROP TABLE space_edit_reports;
DROP TABLE space_change_commands;
DROP TABLE space_change_bases;
DROP TABLE space_change_numbers;
DROP TABLE space_changes;
DELETE FROM sqlite_sequence WHERE name='space_changes';
