//! The SQL functions through which the store's triggers record events:
//! `journal_append(kind, space_id, source)` and
//! `journal_amend_created_base(space_id, base_ref)`.
use anyhow::{Context as _, Result};
use rusqlite::Connection;
use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::types::Null;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::events::{EventKind, FileEventKind};

/// Give `db` the functions the triggers call. Every connection that writes
/// the store registers them; one without them fails on a statement that
/// fires such a trigger.
pub fn register(db: &Connection) -> rusqlite::Result<()> {
    // Neither function is deterministic or innocuous: each writes the
    // journal.
    let flags = FunctionFlags::SQLITE_UTF8;
    db.create_scalar_function("journal_append", 3, flags, |context| {
        let kind: String = context.get(0)?;
        let space: Option<SpaceId> = context.get(1)?;
        let source: String = context.get(2)?;
        with_connection(context, |db| append(db, &kind, space.as_ref(), &source))
    })?;
    db.create_scalar_function("journal_amend_created_base", 2, flags, |context| {
        let space: SpaceId = context.get(0)?;
        let base_ref: String = context.get(1)?;
        with_connection(context, |db| {
            super::amend_created_base(db, &space, &base_ref)
        })
    })
}

/// Run `write` on the connection whose statement called the function,
/// failing that statement with its error.
fn with_connection(
    context: &Context<'_>,
    write: impl FnOnce(&Connection) -> Result<()>,
) -> rusqlite::Result<Null> {
    // SAFETY: SQLite calls the function on the connection's own thread,
    // within the statement whose trigger fired it, and the handle does not
    // outlive this call.
    let db = unsafe { context.get_connection()? };
    write(&db).map_err(|error| rusqlite::Error::UserFunctionError(error.into()))?;
    Ok(Null)
}

/// Record the event of `kind` that a trigger saw in `source`, a row of its
/// table by its column names, for the space `space`; nothing without one.
fn append(db: &Connection, kind: &str, space: Option<&SpaceId>, source: &str) -> Result<()> {
    if let Some(kind) = parse_kind::<FileEventKind>(kind) {
        return super::files::append(db, kind, source);
    }
    let kind =
        parse_kind::<EventKind>(kind).with_context(|| format!("unknown event kind {kind}"))?;
    let Some(space) = space else {
        return Ok(());
    };
    let Some(triggered) = super::sources::event(db, kind, space, source)? else {
        return Ok(());
    };
    super::append_caused(
        db,
        space,
        triggered.event,
        triggered.cause,
        triggered.error.as_deref(),
    )
}

/// The kind `text` names, as the journal stores it.
fn parse_kind<T: serde::de::DeserializeOwned>(text: &str) -> Option<T> {
    serde_json::from_value(text.into()).ok()
}
