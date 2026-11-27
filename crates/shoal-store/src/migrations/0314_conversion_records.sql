-- The migration's preparation moved each conversion into the table with
-- columns, without the fields they hold.
DROP TABLE space_conversions;
-- The trigger that removes a space's conversion with the space already
-- names what the new table becomes, so it is not rewritten for the rename.
PRAGMA legacy_alter_table=ON;
ALTER TABLE space_conversions_columns RENAME TO space_conversions;
PRAGMA legacy_alter_table=OFF;
