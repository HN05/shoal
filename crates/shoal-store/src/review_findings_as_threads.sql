-- A local review's findings are `review` threads Shoal opened, held by the
-- reviewed workspace and numbered in its space after the items it has: the
-- title names the thread, and the body is Shoal's opening post, pointing at
-- the finding's line in the reviewed commit. The workspace's replies are
-- its posts, and an outcome the thread's resolution, a duplicate naming
-- the other finding's thread. A later review's check is Shoal's post; one
-- that found the problem still present reopens the thread, which
-- `review_reopens` records for the review that checked it.
CREATE TABLE review_reopens (
    review_id INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    PRIMARY KEY(review_id, thread_id)
);

CREATE TEMP TABLE finding_threads AS
SELECT f.review_id, f.number AS finding, r.workspace_id, s.number AS space_number,
    s.name AS space_name, s.repository_id, f.severity, f.path, f.line, f.title, f.body,
    f.outcome, f.check_result, f.checked_by, r.head,
    COALESCE(r.ended_at, r.created_at) AS at,
    MAX((SELECT COALESCE(MAX(id), 0) FROM threads),
        (SELECT COALESCE(MAX(seq), 0) FROM sqlite_sequence WHERE name='threads'))
        + ROW_NUMBER() OVER (ORDER BY f.review_id, f.number) AS thread_id,
    (SELECT COALESCE(MAX(held), 0) FROM (
            SELECT number AS held FROM threads WHERE space_number=s.number
            UNION ALL SELECT number FROM access_requests WHERE space_number=s.number))
        + ROW_NUMBER() OVER (PARTITION BY r.workspace_id ORDER BY f.review_id, f.number) AS item
FROM review_findings f JOIN reviews r ON r.id=f.review_id JOIN spaces s ON s.id=r.workspace_id;

-- The outcome each thread takes: none once a check found it still present,
-- and a duplicate's names the thread of the finding it repeats.
ALTER TABLE finding_threads ADD COLUMN resolved TEXT;
UPDATE finding_threads AS f SET resolved=CASE
    WHEN f.check_result='still-present' THEN NULL
    WHEN json_extract(f.outcome,'$.outcome')='duplicate' THEN (
        SELECT json_object('outcome','duplicate','of',json_object('space_number',o.space_number,
            'item',o.item,'number',o.space_number || '.' || o.item))
        FROM finding_threads o
        WHERE o.review_id || '.' || o.finding=json_extract(f.outcome,'$.of'))
    ELSE f.outcome END;

INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener,holder_id,
        outcome,resolved_by,resolver_id,resolved_at,severity,review_id)
    SELECT thread_id, workspace_id, space_number, item, 'review', title, at, 'shoal',
        workspace_id, resolved, CASE WHEN resolved IS NOT NULL THEN 'space' END,
        CASE WHEN resolved IS NOT NULL THEN workspace_id END,
        CASE WHEN resolved IS NOT NULL THEN at END, severity, review_id
    FROM finding_threads ORDER BY thread_id;

INSERT INTO thread_holders(thread_id,space_id)
    SELECT thread_id, workspace_id FROM finding_threads;

INSERT INTO thread_posts(thread_id,created_at,side,sender,text)
    SELECT thread_id, at, 'opener', 'shoal', body FROM finding_threads ORDER BY thread_id;

INSERT INTO thread_code(post_id,space_id,space_name,repository_id,commit_id,path,start,end,
        latest_path,latest_start,latest_end,state)
    SELECT (SELECT MIN(id) FROM thread_posts WHERE thread_id=f.thread_id), f.workspace_id,
        f.space_name, f.repository_id, f.head, f.path, f.line-1, f.line, f.path, f.line-1, f.line,
        'unchanged'
    FROM finding_threads f;

INSERT INTO thread_posts(thread_id,created_at,side,sender,sender_id,sender_name,text)
    SELECT f.thread_id, r.created_at, 'holder', 'space', f.workspace_id, f.space_name, r.body
    FROM review_replies r JOIN finding_threads f ON f.review_id=r.review_id AND f.finding=r.number
    ORDER BY r.id;

INSERT INTO thread_posts(thread_id,created_at,side,sender,text)
    SELECT f.thread_id, COALESCE(checking.ended_at, f.at), 'opener', 'shoal',
        CASE f.check_result
            WHEN 'confirmed' THEN 'Review ' || f.checked_by || ' found it fixed.'
            ELSE 'Review ' || f.checked_by || ' found this still present.'
        END
    FROM finding_threads f LEFT JOIN reviews checking ON checking.id=f.checked_by
    WHERE f.check_result IS NOT NULL
    ORDER BY f.thread_id;

-- A reply went to the next review, and asks the reviewer nothing now: an
-- open thread a reply ends ends with Shoal's post saying so, so it does not
-- wait for the reviewer's answer.
INSERT INTO thread_posts(thread_id,created_at,side,sender,text)
    SELECT p.thread_id, p.created_at, 'opener', 'shoal',
        'The next review read this reply; post again to ask the reviewer.'
    FROM finding_threads f JOIN thread_posts p ON p.id=(SELECT MAX(id) FROM thread_posts
        WHERE thread_id=f.thread_id)
    WHERE f.resolved IS NULL AND p.side='holder'
    ORDER BY f.thread_id;

INSERT INTO review_reopens(review_id,thread_id)
    SELECT f.checked_by, f.thread_id FROM finding_threads f
    WHERE f.check_result='still-present' AND f.checked_by IN (SELECT id FROM reviews);

DROP TABLE finding_threads;
DROP TABLE review_replies;
DROP TABLE review_findings;
