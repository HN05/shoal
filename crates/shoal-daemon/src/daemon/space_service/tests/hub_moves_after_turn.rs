use crate::daemon::hub_moves::after_turn::*;
use crate::daemon::scope::Caller;
use crate::protocol::operations::Ended;
use crate::protocol::operations::Failure;
use crate::transfer::MoveOrder;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::test_support::{self, manager, repository},
    transfer::{MachineDestination, MoveDestination},
};

/// A move of `spaces` to the joined machine desk.
fn order(spaces: &[&SpaceId], keep: bool) -> MoveOrder {
    MoveOrder {
        spaces: spaces.iter().map(|&id| id.clone()).collect(),
        destination: MoveDestination::Machine(MachineDestination {
            machine: "desk".into(),
            resume: true,
        }),
        recursive: false,
        keep,
        checkpoint: None,
        consent: None,
        check_only: false,
    }
}

#[tokio::test]
async fn a_move_waits_for_the_turn_of_the_agent_it_would_stop() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = (manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None))
    .await
    .unwrap();
    let worker = (manager.create_workspace(&repo.id, "worker".into(), None, None, None))
        .await
        .unwrap();
    let below = SpaceSelector::id(&worker.id);
    let child =
        (manager
            .spaces
            .create_child(&*manager, &below, "child".into(), None, Default::default()))
        .await
        .unwrap();
    // A chat in the worker's worktree goes with it.
    let notes = crate::daemon::chats::NewChat {
        priority: None,
        name: "notes".into(),
        agent: "claude".into(),
        mode: "chat".into(),
        narrowing: Default::default(),
        origin: None,
        parent: Some(worker.id.clone()),
    };
    let chat = (manager
        .spaces
        .start_new_chat(&*manager, None, Some(&below), notes))
    .await
    .unwrap();
    let stops = async |caller: &SpaceId, order: &MoveOrder| {
        let caller = Caller::of_space(caller.clone());
        manager
            .spaces
            .move_stops_caller(&*manager, &caller, order)
            .await
            .unwrap()
    };

    assert!(stops(&worker.id, &order(&[&worker.id], false)).await);
    assert!(stops(&chat.id, &order(&[&worker.id], false)).await);
    assert!(!stops(&worker.id, &order(&[&child.id], false)).await);
    assert!(!stops(&worker.id, &order(&[&worker.id], true)).await);
}

#[tokio::test]
async fn a_move_that_left_its_space_here_tells_its_agent_once_it_ended() {
    let (_root, manager) = manager().await;
    let chat = test_support::chat(&manager, "planner", "chat", None, None).await;
    let caller = Caller::of_space(chat.id.clone());
    manager
        .spaces
        .move_after_turn(&caller, order(&[&chat.id], false))
        .await
        .unwrap();
    let space = chat.id.clone();
    (manager.store)
        .write(move |tx| started(tx, &space, "op"))
        .await
        .unwrap();
    let question = "Move chat planner to machine desk".to_owned();
    let ended = Ended::Failed(Failure::Unconfirmed { question });

    manager
        .spaces
        .tell_move_after_turn(&*manager, "op", &ended)
        .await;

    let told = (manager
        .agents
        .agent_messages(&*manager, &SpaceSelector::id(&chat.id)))
    .await
    .unwrap();
    assert!(told[0].message.contains("--yes"), "{}", told[0].message);
    let waiting = manager.store.read(waiting_moves).await.unwrap();
    assert!(waiting.is_empty());
    // A move that ran no recorded move tells no one.
    manager
        .spaces
        .tell_move_after_turn(&*manager, "other", &ended)
        .await;
    let told = (manager
        .agents
        .agent_messages(&*manager, &SpaceSelector::id(&chat.id)))
    .await
    .unwrap();
    assert_eq!(told.len(), 1);
}

#[tokio::test]
async fn a_move_whose_operation_a_restart_cut_off_is_forgotten_and_told() {
    let (_root, manager) = manager().await;
    let chat = test_support::chat(&manager, "planner", "chat", None, None).await;
    let caller = Caller::of_space(chat.id.clone());
    manager
        .spaces
        .move_after_turn(&caller, order(&[&chat.id], false))
        .await
        .unwrap();
    let space = chat.id.clone();
    // As an operation of the daemon before a restart.
    (manager.store)
        .write(move |tx| started(tx, &space, "before-restart"))
        .await
        .unwrap();

    manager
        .spaces
        .finish_interrupted_moves(&*manager)
        .await
        .unwrap();

    assert!(manager.store.read(started_moves).await.unwrap().is_empty());
    let told = (manager
        .agents
        .agent_messages(&*manager, &SpaceSelector::id(&chat.id)))
    .await
    .unwrap();
    assert_eq!(told.len(), 1);
}
