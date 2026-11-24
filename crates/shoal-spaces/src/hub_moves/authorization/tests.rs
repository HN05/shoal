use crate::hub_moves::authorization::moves_without_permission;
use crate::scope::Caller;
use crate::stand_in::Fixture;
use crate::transfer::{MachineDestination, MoveDestination, MoveOrder, SshDestination};
use shoal_daemon_base::callers::Access;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::config::permissions::{Acting, PermissionsConfig};
use shoal_protocol::hub::routing::VouchedSpace;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

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

/// A caller another machine vouches for, with the permissions of the
/// space `like` but `admin`.
async fn vouched_admin(manager: &Fixture, like: &SpaceId, admin: Acting) -> Caller {
    let permissions = manager
        .coordination
        .space_permissions(&**manager, like, None)
        .await
        .unwrap();
    let admin = PermissionsConfig {
        admin: Some(admin),
        ..Default::default()
    };
    let space = SpaceId::generate();
    let vouched = VouchedSpace {
        space: space.clone(),
        everywhere: true,
        permissions: permissions.given(&admin),
        granted: None,
        read_only: false,
        priority: shoal_schedule::priority::Priority::NORMAL,
        chat_modes: Vec::new(),
    };
    Caller {
        space_id: space,
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: Some(Arc::new(vouched)),
    }
}

#[tokio::test]
async fn a_space_without_a_recorded_move_moves_to_no_joined_machine() {
    let manager = Fixture::new().await;
    let chat = manager.chat("planner", "chat", None, None).await;
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
    let manager = Fixture::new().await;
    let chat = manager.chat("planner", "chat", None, None).await;
    let held = order(&[&chat.id], MoveDestination::Held, false);
    let file = MoveDestination::File {
        path: "/nowhere/planner.shoal".into(),
    };
    let file = order(&[&chat.id], file, false);
    let direct = vouched_admin(&manager, &chat.id, Acting::Direct).await;
    let denied = vouched_admin(&manager, &chat.id, Acting::Deny).await;

    let authorize = |caller, order| manager.spaces.authorize_move(&*manager, caller, order);
    authorize(&direct, &held).await.unwrap();
    assert!(authorize(&direct, &file).await.is_err());
    assert!(authorize(&denied, &held).await.is_err());
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
