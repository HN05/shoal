//! How a thread's row holds its outcome: the kind and the cause of a
//! closing in columns, which queries read, and the rest as JSON.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use shoal_protocol::threads::{CloseCause, Outcome, OutcomeKind};

use crate::item_numbers::ItemNumber;

/// What an outcome says beside its kind and cause, which the
/// `outcome_details` column holds.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Details {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    of: Option<ItemNumber>,
}

impl Details {
    fn is_empty(&self) -> bool {
        matches!(
            self,
            Self {
                reason: None,
                issue: None,
                of: None
            }
        )
    }
}

/// The columns of `outcome`: its kind, the cause of a closing, and what
/// else it says as JSON, none when it says nothing else.
pub(super) fn columns(
    outcome: &Outcome,
) -> Result<(OutcomeKind, Option<CloseCause>, Option<String>)> {
    let mut details = Details::default();
    let mut cause = None;
    match outcome.clone() {
        Outcome::Answered | Outcome::Done | Outcome::Fixed => {}
        Outcome::FalsePositive { reason } | Outcome::WontFix { reason } => {
            details.reason = Some(reason);
        }
        Outcome::Withdrawn { reason } => details.reason = reason,
        Outcome::Deferred { issue } => details.issue = Some(issue),
        Outcome::Duplicate { of } => details.of = Some(of),
        Outcome::Closed { cause: closed } => cause = Some(closed),
    }
    let details = match details.is_empty() {
        true => None,
        false => Some(serde_json::to_string(&details)?),
    };
    Ok((outcome.kind(), cause, details))
}

/// The outcome of the kind `kind` that `cause` and `details` complete,
/// refused when they hold what the kind does not take or lack what it does.
fn outcome(
    kind: OutcomeKind,
    cause: Option<CloseCause>,
    details: Option<Details>,
) -> Result<Outcome> {
    let Details { reason, issue, of } = details.unwrap_or_default();
    Ok(match (kind, cause, reason, issue, of) {
        (OutcomeKind::Answered, None, None, None, None) => Outcome::Answered,
        (OutcomeKind::Done, None, None, None, None) => Outcome::Done,
        (OutcomeKind::Fixed, None, None, None, None) => Outcome::Fixed,
        (OutcomeKind::FalsePositive, None, Some(reason), None, None) => {
            Outcome::FalsePositive { reason }
        }
        (OutcomeKind::WontFix, None, Some(reason), None, None) => Outcome::WontFix { reason },
        (OutcomeKind::Deferred, None, None, Some(issue), None) => Outcome::Deferred { issue },
        (OutcomeKind::Duplicate, None, None, None, Some(of)) => Outcome::Duplicate { of },
        (OutcomeKind::Withdrawn, None, reason, None, None) => Outcome::Withdrawn { reason },
        (OutcomeKind::Closed, Some(cause), None, None, None) => Outcome::Closed { cause },
        (kind, ..) => bail!("a {kind} outcome does not take what its thread's row records"),
    })
}

/// The outcome the row `row` of a thread records, none while it is open.
pub(super) fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Outcome>> {
    let Some(kind) = row.get("outcome")? else {
        return Ok(None);
    };
    let details = shoal_store::json_column(row, "outcome_details")?;
    outcome(kind, row.get("cause")?, details)
        .map(Some)
        .map_err(|error| {
            let index = row.as_ref().column_index("outcome").unwrap_or_default();
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                error.into(),
            )
        })
}

#[cfg(test)]
mod tests;
