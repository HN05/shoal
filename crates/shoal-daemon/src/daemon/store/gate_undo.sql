-- What tests that emulate an earlier database run first to undo gate.sql:
-- the gate keeps no sessions or verdicts.
DROP TRIGGER workspace_gate_removed;
DROP TRIGGER chat_gate_removed;
DROP TABLE gate_sessions;
DROP TABLE gate_verdicts;
