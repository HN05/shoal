//! The columns of a question's row that hold its [`QuestionStatus`]: the
//! answer, its time and message, the time it closed and whether its agent
//! withdrew it.
use rusqlite::{Row, types::Type};

use crate::{client::questions::QuestionStatus, time::UnixTime};

/// The filter on a `questions` row aliased `q` that keeps open questions.
pub(super) const OPEN: &str = "q.answer IS NULL AND q.closed_at IS NULL";

/// The status columns, in the order a query selects them.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct StatusColumns {
    pub(super) answer: Option<String>,
    pub(super) answered_at: Option<UnixTime>,
    pub(super) message_id: Option<i64>,
    pub(super) closed_at: Option<UnixTime>,
    pub(super) withdrawn: bool,
}

impl StatusColumns {
    /// The status `row` holds in the columns from `first` on.
    pub(super) fn read(row: &Row<'_>, first: usize) -> rusqlite::Result<QuestionStatus> {
        let columns = Self {
            answer: row.get(first)?,
            answered_at: row.get(first + 1)?,
            message_id: row.get(first + 2)?,
            closed_at: row.get(first + 3)?,
            withdrawn: row.get(first + 4)?,
        };
        columns.status().ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                first + 1,
                Type::Null,
                "a question's answer has no time".into(),
            )
        })
    }

    /// The status the columns hold; `None` for an answer without its time.
    pub(super) fn status(self) -> Option<QuestionStatus> {
        Some(match self {
            Self {
                answer: Some(answer),
                answered_at,
                message_id,
                ..
            } => QuestionStatus::Answered {
                answer,
                answered_at: answered_at?,
                message_id,
            },
            Self {
                closed_at: Some(withdrawn_at),
                withdrawn: true,
                ..
            } => QuestionStatus::Withdrawn { withdrawn_at },
            Self {
                closed_at: Some(closed_at),
                ..
            } => QuestionStatus::Closed { closed_at },
            Self {
                closed_at: None, ..
            } => QuestionStatus::Open,
        })
    }
}

impl From<&QuestionStatus> for StatusColumns {
    fn from(status: &QuestionStatus) -> Self {
        let open = Self {
            answer: None,
            answered_at: None,
            message_id: None,
            closed_at: None,
            withdrawn: false,
        };
        match status {
            QuestionStatus::Open => open,
            QuestionStatus::Answered {
                answer,
                answered_at,
                message_id,
            } => Self {
                answer: Some(answer.clone()),
                answered_at: Some(*answered_at),
                message_id: *message_id,
                ..open
            },
            QuestionStatus::Closed { closed_at } => Self {
                closed_at: Some(*closed_at),
                ..open
            },
            QuestionStatus::Withdrawn { withdrawn_at } => Self {
                closed_at: Some(*withdrawn_at),
                withdrawn: true,
                ..open
            },
        }
    }
}

#[cfg(test)]
mod tests;
