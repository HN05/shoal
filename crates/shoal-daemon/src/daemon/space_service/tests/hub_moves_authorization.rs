use crate::daemon::hub_moves::authorization::*;
use crate::daemon::scope::Caller;
use crate::transfer::MoveDestination;
use crate::transfer::MoveOrder;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

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
        !(manager.spaces.moves_to_joined_machine(&recorded(&chat.id)))
            .await
            .unwrap()
    );
    let gone = SpaceId::from("gone");
    assert!(
        !(manager.spaces.moves_to_joined_machine(&recorded(&gone)))
            .await
            .unwrap()
    );
}

/// A caller on another machine has this machine hold a space's package for
/// it as its `admin` permission allows, and never moves one to a file here.
#[tokio::test]
async fn another_machine_s_caller_has_a_package_held_as_admin_allows() {
    use shoal_protocol::config::permissions::Acting;

    let (_root, manager) = test_support::manager().await;
    let chat = test_support::chat(&manager, "planner", "chat", None, None).await;
    let held = order(&[&chat.id], MoveDestination::Held, false);
    let file = MoveDestination::File {
        path: "/nowhere/planner.shoal".into(),
    };
    let file = order(&[&chat.id], file, false);
    let direct = test_support::vouched_admin(&manager, &chat.id, Acting::Direct).await;
    let denied = test_support::vouched_admin(&manager, &chat.id, Acting::Deny).await;

    let authorize = |caller, order| manager.spaces.authorize_move(&*manager, caller, order);
    authorize(&direct, &held).await.unwrap();
    assert!(authorize(&direct, &file).await.is_err());
    assert!(authorize(&denied, &held).await.is_err());
}
