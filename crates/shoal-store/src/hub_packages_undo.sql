-- What tests that emulate an earlier database run first to undo
-- hub_packages.sql.
DROP TRIGGER hub_package_states_removed;
DROP TABLE hub_package_states;
DROP TRIGGER hub_packages_ended;
DROP TABLE hub_packages;
