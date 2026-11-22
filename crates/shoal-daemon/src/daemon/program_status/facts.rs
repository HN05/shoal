//! What the space's status follows, read from its records together.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _};
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::item_numbers::ItemNumber;

use crate::{
    daemon::{
        agent_queue, approvals, progress, ready_marks, spaces, threads, turn_retry, turn_state,
    },
    state::{AgentState, TurnFailure},
};

pub(super) struct Facts {
    /// The space's name.
    pub(super) name: String,
    /// The newest of its threads whose turn is the user's.
    pub(super) thread: Option<Waiting>,
    /// The oldest of its approval requests the user decides.
    pub(super) approval: Option<Waiting>,
    /// Why the agent's turn failed, when Shoal does not retry it.
    pub(super) turn_failure: Option<TurnFailure>,
    /// Its recorded completion, with its summary.
    pub(super) done: Option<Option<String>>,
    /// Its first ready mark, with the URL of the item it marks.
    pub(super) ready: Option<Option<String>>,
    /// Why Shoal stopped its agent, while it brings the agent back.
    pub(super) asleep: Option<String>,
    /// The turn state the agent's hooks last reported.
    pub(super) turn: Option<AgentState>,
    pub(super) progress: Steps,
}

/// Something of the space that waits for the user.
pub(super) struct Waiting {
    pub(super) number: ItemNumber,
    /// One line saying what it asks.
    pub(super) about: String,
}

/// Where the agent is in its work.
#[derive(Default)]
pub(super) struct Steps {
    /// Its status line.
    pub(super) line: Option<String>,
    pub(super) done: usize,
    pub(super) total: usize,
}

/// The facts of the space `id`, whose agent runs as `execution`, with
/// `pushing` telling which operations still push what the user approved;
/// none once the space is gone.
pub(super) fn read(
    db: &Connection,
    id: &SpaceId,
    execution: &ExecutionId,
    pushing: &dyn Fn(&str) -> bool,
) -> Result<Option<Facts>> {
    let Some(name) = name(db, id)? else {
        return Ok(None);
    };
    Ok(Some(Facts {
        name,
        thread: threads::newest_waiting_for_user(db, id)?.map(|thread| Waiting {
            number: thread.number,
            about: first_line(
                thread
                    .last_post()
                    .map_or(&thread.name, |post| &post.post.text),
            ),
        }),
        approval: approvals::oldest_for_user(db, id, pushing)?.map(|request| Waiting {
            number: request.number,
            about: request.subject(),
        }),
        turn_failure: turn_retry::unretried::failure(db, execution)?,
        done: spaces::done::recorded(db, id)?.map(|completion| completion.summary),
        ready: (ready_marks::list(db, id)?.into_iter().next())
            .map(|mark| mark.url.map(|url| url.url().to_owned())),
        asleep: agent_queue::asleep::current(db, id)?.map(|asleep| asleep.reason),
        turn: turn_state::current(db, id)?.map(|status| status.state),
        progress: progress::current(db, id)?.map_or_else(Steps::default, |progress| Steps {
            done: progress.done(),
            total: progress.steps.len(),
            line: progress.status,
        }),
    }))
}

fn name(db: &Connection, id: &SpaceId) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT name FROM spaces WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?)
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_owned()
}
