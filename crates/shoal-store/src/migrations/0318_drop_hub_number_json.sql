-- The migration's preparation moved each space's ID and number out of the
-- summaries the hub keeps, and each numbering's spares out of its JSON.
ALTER TABLE hub_numbering DROP COLUMN spares;
ALTER TABLE hub_alert_numbering DROP COLUMN spares;

DROP VIEW hub_held_numbers;
DROP TABLE hub_spaces;
DROP TABLE hub_space_copies;
ALTER TABLE hub_spaces_numbered RENAME TO hub_spaces;
ALTER TABLE hub_space_copies_numbered RENAME TO hub_space_copies;

CREATE INDEX hub_spaces_member ON hub_spaces(member_id);

CREATE VIEW hub_held_numbers AS
    SELECT member_id, number, NULL AS space_id FROM hub_reserved_numbers
    UNION ALL SELECT member_id, number, space_id FROM hub_space_copies;

CREATE TRIGGER hub_packages_ended AFTER DELETE ON hub_spaces BEGIN
    DELETE FROM hub_packages
        WHERE space_id=OLD.space_id AND member_id=OLD.member_id;
END;

CREATE TRIGGER hub_packages_handed_over AFTER UPDATE OF member_id ON hub_spaces
    WHEN OLD.member_id <> NEW.member_id BEGIN
    DELETE FROM hub_packages
        WHERE space_id=OLD.space_id AND member_id=OLD.member_id;
END;
