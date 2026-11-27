-- The copies an alert attaches, by path, in the alert's order. A new
-- number the hub gives an alert carries its rows along.
CREATE TABLE alert_attachments (
    alert_id INTEGER NOT NULL REFERENCES alerts(id) ON DELETE CASCADE ON UPDATE CASCADE,
    position INTEGER NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY(alert_id, position)
);

CREATE INDEX alert_attachments_path ON alert_attachments(path);
