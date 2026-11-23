use crate::hub_moves::after_turn::*;
use crate::protocol::operations::Ended;
use crate::protocol::operations::Outcome;
use shoal_ids::space_ids::SpaceId;

#[test]
fn a_carried_space_hears_the_failure_of_the_space_that_carried_it() {
    let (chat, worker): (SpaceId, SpaceId) = ("chat".into(), "worker".into());
    let report = crate::transfer::MoveReport {
        moved: Vec::new(),
        failed: vec![crate::transfer::MoveFailure {
            id: worker,
            name: "worker".into(),
            error: "it changed since its package was written".into(),
        }],
    };
    let ended = Ended::Succeeded(Box::new(Outcome::Moved(report)));
    let told = stayed_message(&chat, &ended);
    assert!(told.contains("it changed since"), "{told}");
}
