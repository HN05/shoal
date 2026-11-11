//! Number the recorded questions and approval requests within their space,
//! where both kinds now share one sequence, and name them that way in the
//! notifications that told the user about them.
use crate::client::space_ids::{SpaceId, SpaceNumber};
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::daemon::item_numbers::{self, ItemNumber};

/// Give every question and request its space's number. Each keeps its own
/// number, which was unique within its kind, so a wait running across the
/// upgrade still finds its question; a request whose number a question of
/// its space holds takes the space's next one instead. A proposed issue of
/// a removed space reserves a number for that space.
pub(super) fn number_items(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("item_numbers.sql"))?;
    reserve_removed_spaces(db)?;
    let mut requests = recorded_requests(db)?;
    separate_kinds(db)?;
    renumber_requests(db, &mut requests)?;
    rewrite_notifications(db, &requests)?;
    db.execute_batch(
        "CREATE UNIQUE INDEX questions_number ON questions(space_number, number);
        CREATE UNIQUE INDEX access_request_number ON access_requests(space_number, number);",
    )?;
    Ok(())
}

/// Give each removed space whose issue proposals wait one number after the
/// numbers of the spaces that exist, in the order it proposed first.
fn reserve_removed_spaces(db: &Connection) -> Result<()> {
    let removed = db
        .prepare(
            "SELECT json_extract(record, '$.space_id') FROM access_requests
                WHERE space_number=0 AND json_valid(record)
                GROUP BY json_extract(record, '$.space_id') ORDER BY MIN(rowid)",
        )?
        .query_map([], |row| row.get::<_, Option<String>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let first: u32 = db.query_row(
        "SELECT COALESCE(MAX(number), 0) + 1 FROM space_numbers",
        [],
        |row| row.get(0),
    )?;
    for (number, space) in (first..).zip(removed) {
        db.execute(
            "UPDATE access_requests SET space_number=?2
                WHERE space_number=0 AND json_valid(record)
                    AND json_extract(record, '$.space_id') IS ?1",
            params![space, number],
        )?;
    }
    Ok(())
}

/// Renumber each request whose number a question of its space holds.
fn separate_kinds(db: &Connection) -> Result<()> {
    let colliding = db
        .prepare(
            "SELECT id,space_number FROM access_requests WHERE EXISTS (SELECT 1 FROM questions
                WHERE questions.space_number=access_requests.space_number
                    AND questions.number=access_requests.number)
            ORDER BY rowid",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, SpaceNumber>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (key, space) in colliding {
        db.execute(
            "UPDATE access_requests SET number=?2 WHERE id=?1",
            params![key, item_numbers::free(db, space)?],
        )?;
    }
    Ok(())
}

/// A request as the upgrade found it: its number then, the space that made
/// it, and its number now.
struct Request {
    previous: i64,
    space_id: Option<SpaceId>,
    number: ItemNumber,
}

/// Each request by its key, with its number before the upgrade.
fn recorded_requests(db: &Connection) -> Result<HashMap<String, Request>> {
    Ok(db
        .prepare(
            "SELECT id,number,COALESCE(workspace_id,
                CASE WHEN json_valid(record) THEN json_extract(record, '$.space_id') END)
            FROM access_requests",
        )?
        .query_map([], |row| {
            let request = Request {
                previous: row.get(1)?,
                space_id: row.get(2)?,
                number: ItemNumber::default(),
            };
            Ok((row.get(0)?, request))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Record each request's number now.
fn renumber_requests(db: &Connection, requests: &mut HashMap<String, Request>) -> Result<()> {
    let numbers = db
        .prepare("SELECT id,space_number,number FROM access_requests")?
        .query_map([], |row| {
            let number = ItemNumber {
                space: row.get(1)?,
                item: row.get(2)?,
            };
            Ok((row.get::<_, String>(0)?, number))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (key, number) in numbers {
        if let Some(request) = requests.get_mut(&key) {
            request.number = number;
        }
    }
    Ok(())
}

/// A notification about a question or a request: what it says, the
/// request it asks about, and the space it names.
struct Notification {
    id: i64,
    kind: String,
    message: String,
    subject: Option<String>,
    space_id: Option<SpaceId>,
    space_number: Option<SpaceNumber>,
}

/// Rewrite the notifications about questions and requests to name their
/// numbers now. A question kept its number within the space the
/// notification names. A request is the one the notification asks about,
/// or else the one of the notification's space that had its number; a
/// number that names neither, as a deleted request's might, is left as it
/// was told.
fn rewrite_notifications(db: &Connection, requests: &HashMap<String, Request>) -> Result<()> {
    let notifications = db
        .prepare(
            "SELECT id,kind,message,subject,workspace_id,COALESCE(
                    (SELECT number FROM workspaces WHERE id=notifications.workspace_id),
                    (SELECT number FROM chats WHERE id=notifications.workspace_id))
            FROM notifications WHERE kind IN
                ('approval_requested','approval_withdrawn','question_asked','question_withdrawn')",
        )?
        .query_map([], |row| {
            Ok(Notification {
                id: row.get(0)?,
                kind: row.get(1)?,
                message: row.get(2)?,
                subject: row.get(3)?,
                space_id: row.get(4)?,
                space_number: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for notification in notifications {
        let rewritten = match notification.kind.as_str() {
            "question_asked" | "question_withdrawn" => {
                renumbered(&notification.message, QUESTION_FORMS, |old| {
                    Some(ItemNumber {
                        space: notification.space_number?,
                        item: u32::try_from(old).ok()?,
                    })
                })
            }
            _ => renumbered(&notification.message, APPROVAL_FORMS, |old| {
                request_named(requests, &notification, old)
            }),
        };
        if let Some(rewritten) = rewritten {
            db.execute(
                "UPDATE notifications SET message=?2 WHERE id=?1",
                params![notification.id, rewritten],
            )?;
        }
    }
    Ok(())
}

/// The number now of the request `notification` named by its number
/// `old` before the upgrade.
fn request_named(
    requests: &HashMap<String, Request>,
    notification: &Notification,
    old: i64,
) -> Option<ItemNumber> {
    let request = match &notification.subject {
        Some(key) => requests.get(key)?,
        None => requests.values().find(|request| {
            request.previous == old
                && request.space_id.is_some()
                && request.space_id == notification.space_id
        })?,
    };
    (request.previous == old).then_some(request.number)
}

/// How notifications about questions named their number: the first form
/// starts the message and gives the number; each occurrence of every form
/// is rewritten.
const QUESTION_FORMS: &[&str] = &["question ", "withdrew question ", "shoal questions "];
const APPROVAL_FORMS: &[&str] = &["approval ", "access ", "--approve "];

/// `message` with the number its forms name replaced by the number `new`
/// maps it to, or `None` when it names none `new` knows.
fn renumbered(
    message: &str,
    forms: &[&str],
    new: impl Fn(i64) -> Option<ItemNumber>,
) -> Option<String> {
    let old = forms.iter().find_map(|form| {
        let rest = message.strip_prefix(form)?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<i64>().ok()
    })?;
    let new = new(old)?;
    let replacements: Vec<_> = forms
        .iter()
        .map(|form| (format!("{form}{old}"), format!("{form}{new}")))
        .collect();
    Some(replace_numbers(message, &replacements))
}

/// `text` with each `from` that no further digit follows replaced by its
/// `to`, in one pass, so a replacement is never rewritten again.
fn replace_numbers(text: &str, replacements: &[(String, String)]) -> String {
    let mut result = String::new();
    let mut rest = text;
    while let Some(first) = rest.chars().next() {
        let found = replacements.iter().find(|(from, _)| {
            rest.strip_prefix(from.as_str())
                .is_some_and(|after| !after.starts_with(|c: char| c.is_ascii_digit()))
        });
        match found {
            Some((from, to)) => {
                result.push_str(to);
                rest = &rest[from.len()..];
            }
            None => {
                result.push(first);
                rest = &rest[first.len_utf8()..];
            }
        }
    }
    result
}

#[cfg(test)]
mod tests;
