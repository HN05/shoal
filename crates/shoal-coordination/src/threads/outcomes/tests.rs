use anyhow::Result;
use shoal_protocol::threads::{CloseCause, Outcome};

use super::*;

#[test]
fn each_outcome_comes_back_from_its_columns() -> Result<()> {
    let of = ItemNumber {
        space: 3.into(),
        item: 2,
    };
    for recorded in [
        Outcome::Answered,
        Outcome::Done,
        Outcome::Fixed,
        Outcome::FalsePositive {
            reason: "the caller checks".into(),
        },
        Outcome::WontFix {
            reason: "too costly".into(),
        },
        Outcome::Deferred {
            issue: "#12".into(),
        },
        Outcome::Duplicate { of },
        Outcome::Withdrawn { reason: None },
        Outcome::Withdrawn {
            reason: Some("merged".into()),
        },
        Outcome::Closed {
            cause: CloseCause::Removal,
        },
    ] {
        let (kind, cause, details) = columns(&recorded)?;
        let details = details
            .map(|text| serde_json::from_str(&text))
            .transpose()?;
        assert_eq!(outcome(kind, cause, details)?, recorded);
    }
    Ok(())
}

#[test]
fn columns_that_do_not_fit_the_kind_are_refused() {
    let reason = || {
        Some(Details {
            reason: Some("why".into()),
            ..Details::default()
        })
    };
    assert!(outcome(OutcomeKind::Answered, None, reason()).is_err());
    assert!(outcome(OutcomeKind::WontFix, None, None).is_err());
    assert!(outcome(OutcomeKind::Closed, None, None).is_err());
    assert!(outcome(OutcomeKind::Done, Some(CloseCause::Completion), None).is_err());
}
