-- What a space last saw of an item keeps the item's state in a column of
-- its own, which ranking reads. The next migration's preparation fills it
-- from the record.
ALTER TABLE pr_activity ADD COLUMN state TEXT;
ALTER TABLE space_item_activity ADD COLUMN state TEXT;
