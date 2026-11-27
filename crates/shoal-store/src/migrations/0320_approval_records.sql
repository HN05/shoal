-- The requests with their columns, which the migration's preparation
-- filled, replace those that kept every field in the record. The views and
-- triggers that name the table keep naming it: the legacy rename neither
-- rewrites them nor checks them while it is gone.
DROP TABLE access_requests;
PRAGMA legacy_alter_table=ON;
ALTER TABLE access_request_columns RENAME TO access_requests;
PRAGMA legacy_alter_table=OFF;

CREATE UNIQUE INDEX access_request_name
    ON access_requests(space_id,target_key,name) WHERE active=1;
CREATE INDEX access_request_target ON access_requests(space_id,target_key);
CREATE UNIQUE INDEX access_request_number ON access_requests(space_number, number);
CREATE INDEX access_request_repository ON access_requests(repository_id);
CREATE INDEX access_request_decider ON access_requests(decider) WHERE decider IS NOT NULL;
CREATE INDEX access_request_proposer ON access_requests(proposer_id)
    WHERE proposer_id IS NOT NULL;
