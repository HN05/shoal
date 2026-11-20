//! The PRs a space assigns, as its PR watch record names them, and their
//! titles.
use rusqlite::{Connection, OptionalExtension};
use shoal_forge_types::items::reference::ItemRef;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::links::pr::{Registration, RegistrationKind};

pub mod titles;

/// The PRs the space `id` watches; an unreadable registration watches none.
pub fn watched_prs(db: &Connection, id: &SpaceId) -> rusqlite::Result<Vec<ItemRef>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM pr_cleanup WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let registration = record.and_then(|record| serde_json::from_str(&record).ok());
    Ok(watched_urls(registration.as_ref()).to_vec())
}

/// The PRs `registration` watches, none without one.
pub fn watched_urls(registration: Option<&Registration>) -> &[ItemRef] {
    match registration {
        Some(Registration {
            kind: RegistrationKind::Watch { urls, .. },
            ..
        }) => urls,
        _ => &[],
    }
}
