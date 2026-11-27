-- The reports the daemon applied, by the ID their sender gave them, so that
-- a report sent again after its acknowledgement was lost applies once.
CREATE TABLE applied_reports (
    id TEXT PRIMARY KEY,
    applied_at INTEGER NOT NULL DEFAULT (unixepoch())
) WITHOUT ROWID;
