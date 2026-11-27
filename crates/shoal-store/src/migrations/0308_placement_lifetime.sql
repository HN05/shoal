-- A placement request names its lifetime. The requests the hub keeps with
-- packages were all placed as lasting work.
UPDATE hub_packages SET start=json_set(start,'$.lifetime','lasting')
WHERE start IS NOT NULL AND json_valid(start);
