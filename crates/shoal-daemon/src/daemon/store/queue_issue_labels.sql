-- A queued launch records the labels of the issue it starts on, as a JSON
-- array, whose areas order it in the queue until its workspace exists.
ALTER TABLE agent_queue ADD COLUMN issue_labels TEXT;
