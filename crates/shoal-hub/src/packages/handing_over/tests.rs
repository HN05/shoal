use anyhow::anyhow;
use shoal_schedule::placement::{Policy, Reason};

use super::*;

fn desk() -> Decision {
    Decision {
        machine: "desk".into(),
        name: "desk".into(),
        reason: Reason::NoHeavyAgents,
        policy: Policy::Balanced,
        unanswered: None,
        skipped: Vec::new(),
    }
}

#[test]
fn a_release_the_hub_may_have_taken_leaves_the_space_and_one_it_surely_did_not_keeps_it() {
    let went = |released| matches!(released_as(released, desk()), Handed::Went(_));
    let unsure = |released| matches!(released_as(released, desk()), Handed::Unconfirmed(_));
    let stays = |released| matches!(released_as(released, desk()), Handed::Stayed(_));
    assert!(went(Some(Ok(Reply::Delivered))));
    // Its answer never came, in time or at all.
    assert!(unsure(None));
    assert!(unsure(Some(Err(Unanswered("no answer".into()).into()))));
    // It refused the release, as with no placement waiting.
    assert!(stays(Some(Err(anyhow!("no hand-over waits")))));
}
