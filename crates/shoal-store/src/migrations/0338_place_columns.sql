-- A place's host, ID and instance move out of the JSON that executions and
-- owned places keep of it into columns of their own, which the next
-- migration's preparation fills. An owned place's columns held its host and
-- ID already.
ALTER TABLE executions ADD COLUMN origin_host TEXT;
ALTER TABLE executions ADD COLUMN origin_place TEXT;
ALTER TABLE executions ADD COLUMN origin_instance TEXT;
ALTER TABLE owned_places RENAME COLUMN host TO origin_host;
ALTER TABLE owned_places RENAME COLUMN place TO origin_place;
ALTER TABLE owned_places ADD COLUMN origin_instance TEXT;
