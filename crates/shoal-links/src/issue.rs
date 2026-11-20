//! The issues a space assigns, as its records hold them.
use rusqlite::Connection;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::{Assigner, IssueAssignee};

/// The forge assignees the space `id`'s assigned issues stand for, in the
/// order the issues were assigned: the user's login it stands for, and the
/// agent account for each issue an agent assigned it.
pub fn assignees_of(db: &Connection, id: &SpaceId) -> rusqlite::Result<Vec<IssueAssignee>> {
    db.prepare(
        "SELECT url,?2,EXISTS(SELECT 1 FROM issue_assignments a
             WHERE a.url=s.url AND a.login=s.assignee), rowid
         FROM space_issue s WHERE space_id=?1 AND assignee IS NOT NULL
         UNION ALL SELECT url,linked_by,0,rowid FROM space_issue
         WHERE space_id=?1 AND linked_by=?3 ORDER BY 4",
    )?
    .query_map(
        rusqlite::params![id, Assigner::User, Assigner::Agent],
        |row| {
            Ok(IssueAssignee {
                url: row.get(0)?,
                account: row.get(1)?,
                added: row.get(2)?,
            })
        },
    )?
    .collect()
}
