use super::*;
use crate::{
    daemon::test_support,
    transfer::{MachineDestination, SshDestination},
};

/// A move of `spaces` to `destination`, or copies of them with `keep`.
fn order(spaces: &[&SpaceId], destination: MoveDestination, keep: bool) -> MoveOrder {
    MoveOrder {
        spaces: spaces.iter().map(|&id| id.clone()).collect(),
        destination,
        recursive: false,
        keep,
        checkpoint: None,
        consent: None,
        check_only: false,
    }
}

fn joined() -> MoveDestination {
    MoveDestination::Machine(MachineDestination {
        machine: "desk".into(),
        resume: true,
    })
}

#[test]
fn only_a_copy_of_the_caller_s_own_space_there_takes_a_permission() {
    let (own, child): (SpaceId, SpaceId) = ("own".into(), "child".into());
    let caller = Caller::of_space(own.clone());
    assert!(moves_without_permission(
        &caller,
        &order(&[&own], joined(), false)
    ));
    assert!(moves_without_permission(
        &caller,
        &order(&[&child], joined(), true)
    ));
    assert!(!moves_without_permission(
        &caller,
        &order(&[&own], joined(), true)
    ));
    let ssh = MoveDestination::Ssh(SshDestination {
        host: "desk".into(),
        shoal: None,
        resume: true,
    });
    assert!(!moves_without_permission(
        &caller,
        &order(&[&child], ssh, false)
    ));
}

#[tokio::test]
async fn a_space_without_a_recorded_move_moves_to_no_joined_machine() {
    let (_root, manager) = test_support::manager().await;
    let chat = test_support::chat(&manager, "planner", "chat", None, None).await;
    let recorded = |id: &SpaceId| SpaceSelector::id(id);
    assert!(
        !(manager.moves_to_joined_machine(&recorded(&chat.id)))
            .await
            .unwrap()
    );
    let gone = SpaceId::from("gone");
    assert!(
        !(manager.moves_to_joined_machine(&recorded(&gone)))
            .await
            .unwrap()
    );
}
