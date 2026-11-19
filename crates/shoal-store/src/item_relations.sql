-- A space links each item under one relation: an assignment replaces the
-- item's opened or related link, and an item both opened and related stays
-- opened.
DELETE FROM space_items WHERE EXISTS (
    SELECT 1 FROM space_issue
    WHERE space_issue.space_id=space_items.space_id AND space_issue.url=space_items.url
) OR EXISTS (
    SELECT 1 FROM pr_cleanup,
        json_each(CASE WHEN json_valid(record) THEN record ELSE '{}' END, '$.urls') AS watched
    WHERE pr_cleanup.space_id=space_items.space_id AND watched.value=space_items.url
) OR EXISTS (
    SELECT 1 FROM pr_cleanup
    WHERE pr_cleanup.space_id=space_items.space_id
        AND CASE WHEN json_valid(record) THEN json_extract(record, '$.url') END=space_items.url
);
DELETE FROM space_items WHERE relation='related' AND EXISTS (
    SELECT 1 FROM space_items AS opened
    WHERE opened.space_id=space_items.space_id AND opened.url=space_items.url
        AND opened.relation='opened'
);
