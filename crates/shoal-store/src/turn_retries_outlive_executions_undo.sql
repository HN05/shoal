-- What tests that emulate an earlier database run first to undo
-- turn_retries_outlive_executions.sql: an execution's end drops its retry
-- through the table's reference, which space_turn_retries_undo.sql restores.
DROP TRIGGER space_turn_retry_execution_ended;
