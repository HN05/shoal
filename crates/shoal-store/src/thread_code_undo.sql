-- What tests that emulate an earlier database run first to undo
-- thread_code.sql: posts point at no code.
DROP TABLE thread_code;
DELETE FROM sqlite_sequence WHERE name='thread_code';
