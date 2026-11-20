use anyhow::{Context as _, anyhow};

use super::*;

#[test]
fn a_claim_whose_answer_never_came_may_stand_and_any_other_failure_took_none() {
    let lost: anyhow::Result<()> = Err(session::Unanswered("the link closed".into()).into());
    let lost = lost.context("claim space 7").unwrap_err();
    assert!(matches!(ClaimFailure::of(lost), ClaimFailure::Unknown(_)));
    let refused = anyhow!("the machine the space moves from gave the move up");
    assert!(matches!(
        ClaimFailure::of(refused),
        ClaimFailure::Refused(_)
    ));
}
