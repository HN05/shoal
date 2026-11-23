-- What tests that emulate an earlier database run first to undo
-- hub_user_alerts.sql and the migrations after it that change its table.
DROP TABLE hub_user_alerts;
