-- What tests that emulate an earlier database run first to undo
-- hub_routing.sql.
DROP TABLE hub_escalated_questions;
DROP TABLE hub_outbox;
DROP TABLE hub_queue;
