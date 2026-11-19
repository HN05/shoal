-- The title of each PR a space is assigned, when known. The space's PR
-- watch record, which drives cleanup, names the PRs; a row goes with its
-- PR's watch and with the space.
CREATE TABLE space_pr_titles (
    space_id TEXT NOT NULL,
    url TEXT NOT NULL,
    title TEXT NOT NULL,
    PRIMARY KEY(space_id,url)
);

CREATE TRIGGER space_pr_titles_removed AFTER DELETE ON space_records BEGIN
    DELETE FROM space_pr_titles WHERE space_id=OLD.id;
END;
